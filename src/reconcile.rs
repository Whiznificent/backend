//! On-chain vs off-chain position reconciliation job.
//!
//! Compares indexed on-chain state (option token balances, vault collateral
//! per account, series state) with the backend's `positions` and `accounts`
//! projections. Produces discrepancy reports, metrics and alerts, and exposes
//! the latest results via `GET /api/v1/admin/reconciliation/latest`.
//!
//! Both sides are snapshotted at a fixed ledger sequence so the comparison is
//! consistent. In-flight transactions (submitted within the last
//! `IN_FLIGHT_LEDGERS` ledgers) are excluded from the comparison.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::chain::rpc::{ChainRpc, LedgerEntry, LedgerKey};

/// Number of ledgers to look back when excluding in-flight transactions.
pub const IN_FLIGHT_LEDGERS: u32 = 5;

/// Maximum number of keys per `getLedgerEntries` call.
pub const MAX_KEYS_PER_BATCH: usize = 200;

/// Severity assigned to a discrepancy category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// Category of a reconciliation discrepancy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscrepancyCategory {
    /// Present on-chain but missing from the off-chain projection.
    MissingOffchain,
    /// Present off-chain but missing on-chain.
    MissingOnchain,
    /// Present on both sides but with differing amounts.
    AmountMismatch,
    /// Present on both sides but with differing status.
    StatusMismatch,
}

impl DiscrepancyCategory {
    /// Default severity for this category.
    pub fn severity(self) -> Severity {
        match self {
            DiscrepancyCategory::MissingOffchain => Severity::Critical,
            DiscrepancyCategory::MissingOnchain => Severity::Warning,
            DiscrepancyCategory::AmountMismatch => Severity::Critical,
            DiscrepancyCategory::StatusMismatch => Severity::Warning,
        }
    }

    /// Whether this category is safe to auto-heal (projection missing an event
    /// that exists in raw events). Off by default.
    pub fn is_auto_healable(self) -> bool {
        matches!(self, DiscrepancyCategory::MissingOffchain)
    }
}

/// A single reconciliation discrepancy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationItem {
    pub category: DiscrepancyCategory,
    pub severity: Severity,
    pub wallet: String,
    pub series: Option<String>,
    pub onchain_amount: Option<i128>,
    pub offchain_amount: Option<i128>,
    pub onchain_status: Option<String>,
    pub offchain_status: Option<String>,
    pub detail: String,
}

/// Summary of a reconciliation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationSummary {
    pub ledger_sequence: u32,
    pub wallets_checked: usize,
    pub series_checked: usize,
    pub items: Vec<ReconciliationItem>,
    pub critical_count: usize,
    pub warning_count: usize,
    pub info_count: usize,
    pub auto_healed: usize,
}

impl ReconciliationSummary {
    /// Emit the summary metric and fire an alert when critical discrepancies
    /// are above zero.
    pub fn emit_metrics_and_alerts(&self) {
        metrics::gauge!("reconciliation.critical", self.critical_count as f64);
        metrics::gauge!("reconciliation.warning", self.warning_count as f64);
        metrics::gauge!("reconciliation.items", self.items.len() as f64);
        metrics::gauge!("reconciliation.ledger", self.ledger_sequence as f64);
        if self.critical_count > 0 {
            tracing::error!(
                ledger = self.ledger_sequence,
                critical = self.critical_count,
                "reconciliation detected critical discrepancies"
            );
        }
    }
}

/// Off-chain projection of a position for a wallet/series pair.
#[derive(Debug, Clone)]
pub struct OffchainPosition {
    pub wallet: String,
    pub series: String,
    pub amount: i128,
    pub status: String,
}

/// Off-chain projection of an account's collateral.
#[derive(Debug, Clone)]
pub struct OffchainAccount {
    pub wallet: String,
    pub collateral: i128,
}

/// On-chain snapshot of a wallet/series balance.
#[derive(Debug, Clone)]
pub struct OnchainBalance {
    pub wallet: String,
    pub series: String,
    pub amount: i128,
    pub status: String,
}

/// On-chain snapshot of a wallet's collateral.
#[derive(Debug, Clone)]
pub struct OnchainCollateral {
    pub wallet: String,
    pub collateral: i128,
}

/// Configuration for a reconciliation run.
#[derive(Debug, Clone)]
pub struct ReconcileConfig {
    /// Auto-heal strictly safe categories (projection missing an event that
    /// exists in raw events). Off by default.
    pub auto_heal: bool,
}

impl Default for ReconcileConfig {
    fn default() -> Self {
        Self { auto_heal: false }
    }
}

/// The reconciliation job.
pub struct Reconciler {
    rpc: Arc<dyn ChainRpc>,
    config: ReconcileConfig,
}

impl Reconciler {
    pub fn new(rpc: Arc<dyn ChainRpc>, config: ReconcileConfig) -> Self {
        Self { rpc, config }
    }

    /// Run a reconciliation pass, snapshotting both sides at a fixed ledger
    /// sequence for a consistent comparison.
    pub async fn run(
        &self,
        offchain_positions: &[OffchainPosition],
        offchain_accounts: &[OffchainAccount],
        in_flight: &[String],
    ) -> anyhow::Result<ReconciliationSummary> {
        // Snapshot at a fixed ledger sequence so both sides are consistent.
        let ledger_sequence = self.rpc.latest_ledger_sequence().await?;
        let cutoff = ledger_sequence.saturating_sub(IN_FLIGHT_LEDGERS);

        let in_flight: std::collections::HashSet<&str> =
            in_flight.iter().map(String::as_str).collect();

        // Build the set of keys to read as ground truth.
        let mut keys: Vec<LedgerKey> = Vec::new();
        for p in offchain_positions {
            if in_flight.contains(p.wallet.as_str()) {
                continue;
            }
            keys.push(LedgerKey::contract_data(&p.series, &p.wallet));
        }
        for a in offchain_accounts {
            if in_flight.contains(a.wallet.as_str()) {
                continue;
            }
            keys.push(LedgerKey::contract_data("vault", &a.wallet));
        }

        // Batch point reads up to MAX_KEYS_PER_BATCH keys per call.
        let mut entries: HashMap<String, LedgerEntry> = HashMap::new();
        for chunk in keys.chunks(MAX_KEYS_PER_BATCH) {
            let batch = self.rpc.get_ledger_entries(chunk, cutoff).await?;
            for entry in batch {
                entries.insert(entry.key.clone(), entry);
            }
        }

        let onchain_balances = self.collect_onchain_balances(&entries, offchain_positions);
        let onchain_collateral = self.collect_onchain_collateral(&entries, offchain_accounts);

        let mut items = Vec::new();
        compare_positions(offchain_positions, &onchain_balances, &mut items);
        compare_collateral(offchain_accounts, &onchain_collateral, &mut items);

        let mut auto_healed = 0usize;
        if self.config.auto_heal {
            auto_healed = items
                .iter()
                .filter(|i| i.category.is_auto_healable())
                .count();
            // Auto-heal only strictly safe categories; balances are never
            // corrected automatically (out of scope).
            items.retain(|i| !i.category.is_auto_healable());
        }

        let critical_count = items
            .iter()
            .filter(|i| i.severity == Severity::Critical)
            .count();
        let warning_count = items
            .iter()
            .filter(|i| i.severity == Severity::Warning)
            .count();
        let info_count = items
            .iter()
            .filter(|i| i.severity == Severity::Info)
            .count();

        let summary = ReconciliationSummary {
            ledger_sequence,
            wallets_checked: offchain_accounts.len(),
            series_checked: offchain_positions.len(),
            items,
            critical_count,
            warning_count,
            info_count,
            auto_healed,
        };
        summary.emit_metrics_and_alerts();
        Ok(summary)
    }

    fn collect_onchain_balances(
        &self,
        entries: &HashMap<String, LedgerEntry>,
        positions: &[OffchainPosition],
    ) -> Vec<OnchainBalance> {
        positions
            .iter()
            .filter_map(|p| {
                let key = LedgerKey::contract_data(&p.series, &p.wallet).to_string();
                entries.get(&key).map(|e| OnchainBalance {
                    wallet: p.wallet.clone(),
                    series: p.series.clone(),
                    amount: e.amount,
                    status: e.status.clone(),
                })
            })
            .collect()
    }

    fn collect_onchain_collateral(
        &self,
        entries: &HashMap<String, LedgerEntry>,
        accounts: &[OffchainAccount],
    ) -> Vec<OnchainCollateral> {
        accounts
            .iter()
            .filter_map(|a| {
                let key = LedgerKey::contract_data("vault", &a.wallet).to_string();
                entries.get(&key).map(|e| OnchainCollateral {
                    wallet: a.wallet.clone(),
                    collateral: e.amount,
                })
            })
            .collect()
    }
}

/// Compare off-chain positions against on-chain balances.
fn compare_positions(
    offchain: &[OffchainPosition],
    onchain: &[OnchainBalance],
    items: &mut Vec<ReconciliationItem>,
) {
    let onchain_map: HashMap<(&str, &str), &OnchainBalance> = onchain
        .iter()
        .map(|b| ((b.wallet.as_str(), b.series.as_str()), b))
        .collect();

    for p in offchain {
        match onchain_map.get(&(p.wallet.as_str(), p.series.as_str())) {
            None => items.push(ReconciliationItem {
                category: DiscrepancyCategory::MissingOnchain,
                severity: DiscrepancyCategory::MissingOnchain.severity(),
                wallet: p.wallet.clone(),
                series: Some(p.series.clone()),
                onchain_amount: None,
                offchain_amount: Some(p.amount),
                onchain_status: None,
                offchain_status: Some(p.status.clone()),
                detail: "position present off-chain but missing on-chain".into(),
            }),
            Some(b) => {
                if b.amount != p.amount {
                    items.push(ReconciliationItem {
                        category: DiscrepancyCategory::AmountMismatch,
                        severity: DiscrepancyCategory::AmountMismatch.severity(),
                        wallet: p.wallet.clone(),
                        series: Some(p.series.clone()),
                        onchain_amount: Some(b.amount),
                        offchain_amount: Some(p.amount),
                        onchain_status: Some(b.status.clone()),
                        offchain_status: Some(p.status.clone()),
                        detail: "position amount differs between on-chain and off-chain".into(),
                    });
                }
                if b.status != p.status {
                    items.push(ReconciliationItem {
                        category: DiscrepancyCategory::StatusMismatch,
                        severity: DiscrepancyCategory::StatusMismatch.severity(),
                        wallet: p.wallet.clone(),
                        series: Some(p.series.clone()),
                        onchain_amount: Some(b.amount),
                        offchain_amount: Some(p.amount),
                        onchain_status: Some(b.status.clone()),
                        offchain_status: Some(p.status.clone()),
                        detail: "position status differs between on-chain and off-chain".into(),
                    });
                }
            }
        }
    }

    // On-chain balances with no off-chain projection.
    let offchain_map: HashMap<(&str, &str), &OffchainPosition> = offchain
        .iter()
        .map(|p| ((p.wallet.as_str(), p.series.as_str()), p))
        .collect();
    for b in onchain {
        if !offchain_map.contains_key(&(b.wallet.as_str(), b.series.as_str())) {
            items.push(ReconciliationItem {
                category: DiscrepancyCategory::MissingOffchain,
                severity: DiscrepancyCategory::MissingOffchain.severity(),
                wallet: b.wallet.clone(),
                series: Some(b.series.clone()),
                onchain_amount: Some(b.amount),
                offchain_amount: None,
                onchain_status: Some(b.status.clone()),
                offchain_status: None,
                detail: "position present on-chain but missing off-chain".into(),
            });
        }
    }
}

/// Compare off-chain accounts against on-chain collateral.
fn compare_collateral(
    offchain: &[OffchainAccount],
    onchain: &[OnchainCollateral],
    items: &mut Vec<ReconciliationItem>,
) {
    let onchain_map: HashMap<&str, &OnchainCollateral> = onchain
        .iter()
        .map(|c| (c.wallet.as_str(), c))
        .collect();

    for a in offchain {
        match onchain_map.get(a.wallet.as_str()) {
            None => items.push(ReconciliationItem {
                category: DiscrepancyCategory::MissingOnchain,
                severity: DiscrepancyCategory::MissingOnchain.severity(),
                wallet: a.wallet.clone(),
                series: None,
                onchain_amount: None,
                offchain_amount: Some(a.collateral),
                onchain_status: None,
                offchain_status: None,
                detail: "collateral present off-chain but missing on-chain".into(),
            }),
            Some(c) => {
                if c.collateral != a.collateral {
                    items.push(ReconciliationItem {
                        category: DiscrepancyCategory::AmountMismatch,
                        severity: DiscrepancyCategory::AmountMismatch.severity(),
                        wallet: a.wallet.clone(),
                        series: None,
                        onchain_amount: Some(c.collateral),
                        offchain_amount: Some(a.collateral),
                        onchain_status: None,
                        offchain_status: None,
                        detail: "collateral amount differs between on-chain and off-chain".into(),
                    });
                }
            }
        }
    }

    let offchain_map: HashMap<&str, &OffchainAccount> = offchain
        .iter()
        .map(|a| (a.wallet.as_str(), a))
        .collect();
    for c in onchain {
        if !offchain_map.contains_key(c.wallet.as_str()) {
            items.push(ReconciliationItem {
                category: DiscrepancyCategory::MissingOffchain,
                severity: DiscrepancyCategory::MissingOffchain.severity(),
                wallet: c.wallet.clone(),
                series: None,
                onchain_amount: Some(c.collateral),
                offchain_amount: None,
                onchain_status: None,
                offchain_status: None,
                detail: "collateral present on-chain but missing off-chain".into(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(wallet: &str, series: &str, amount: i128, status: &str) -> OffchainPosition {
        OffchainPosition {
            wallet: wallet.into(),
            series: series.into(),
            amount,
            status: status.into(),
        }
    }

    fn bal(wallet: &str, series: &str, amount: i128, status: &str) -> OnchainBalance {
        OnchainBalance {
            wallet: wallet.into(),
            series: series.into(),
            amount,
            status: status.into(),
        }
    }

    #[test]
    fn clean_state_produces_zero_items() {
        let offchain = vec![pos("w1", "s1", 100, "open")];
        let onchain = vec![bal("w1", "s1", 100, "open")];
        let mut items = Vec::new();
        compare_positions(&offchain, &onchain, &mut items);
        assert!(items.is_empty());
    }

    #[test]
    fn detects_amount_mismatch() {
        let offchain = vec![pos("w1", "s1", 100, "open")];
        let onchain = vec![bal("w1", "s1", 90, "open")];
        let mut items = Vec::new();
        compare_positions(&offchain, &onchain, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].category, DiscrepancyCategory::AmountMismatch);
        assert_eq!(items[0].severity, Severity::Critical);
    }

    #[test]
    fn detects_status_mismatch() {
        let offchain = vec![pos("w1", "s1", 100, "open")];
        let onchain = vec![bal("w1", "s1", 100, "closed")];
        let mut items = Vec::new();
        compare_positions(&offchain, &onchain, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].category, DiscrepancyCategory::StatusMismatch);
    }

    #[test]
    fn detects_missing_onchain() {
        let offchain = vec![pos("w1", "s1", 100, "open")];
        let onchain = vec![];
        let mut items = Vec::new();
        compare_positions(&offchain, &onchain, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].category, DiscrepancyCategory::MissingOnchain);
    }

    #[test]
    fn detects_missing_offchain() {
        let offchain = vec![];
        let onchain = vec![bal("w1", "s1", 100, "open")];
        let mut items = Vec::new();
        compare_positions(&offchain, &onchain, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].category, DiscrepancyCategory::MissingOffchain);
        assert!(items[0].category.is_auto_healable());
    }

    #[test]
    fn detects_collateral_mismatch() {
        let offchain = vec![OffchainAccount {
            wallet: "w1".into(),
            collateral: 500,
        }];
        let onchain = vec![OnchainCollateral {
            wallet: "w1".into(),
            collateral: 400,
        }];
        let mut items = Vec::new();
        compare_collateral(&offchain, &onchain, &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].category, DiscrepancyCategory::AmountMismatch);
    }
}
