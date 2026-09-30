//! Dynamic collateral re-margining on spot moves.
//!
//! Open short positions lock collateral at the spot price observed when the
//! position was opened. As spot moves, the liability of a short call changes
//! but the locked collateral does not, leaving the account under-collateralised
//! on a rise and over-collateralised on a fall. This module recomputes the
//! required collateral for open short positions and moves the difference
//! between free balance and locked collateral, within hysteresis bands, while
//! recording every adjustment for audit.
//!
//! The process is exposed as [`remargin_once`] so it can be driven by a
//! background loop in the same testable style as the other engine loops.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Configuration for the re-margining process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemarginConfig {
    /// Fractional drift (e.g. `0.10` for 10%) beyond which a re-margin is
    /// triggered. Drift below this band is ignored to prevent flapping.
    pub drift_threshold: f64,
    /// Fractional band (e.g. `0.02` for 2%) around the required collateral
    /// inside which no adjustment is made, even if the drift threshold is met.
    pub hysteresis_band: f64,
    /// Maximum number of positions processed per run.
    pub batch_size: usize,
}

impl Default for RemarginConfig {
    fn default() -> Self {
        Self {
            drift_threshold: 0.10,
            hysteresis_band: 0.02,
            batch_size: 256,
        }
    }
}

/// A single open short position eligible for re-margining.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShortPosition {
    pub position_id: String,
    pub wallet: String,
    /// Collateral currently locked for this position.
    pub locked_collateral: f64,
    /// Spot price observed when the position was opened.
    pub entry_spot: f64,
    /// Quantity of the underlying the short is exposed to.
    pub quantity: f64,
    /// Whether the position is still open. Closed positions are skipped.
    pub is_open: bool,
}

/// A wallet's balances relevant to re-margining.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletBalance {
    pub wallet: String,
    pub free_balance: f64,
    pub locked_collateral: f64,
}

/// The kind of adjustment applied to a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdjustmentKind {
    /// Collateral was added from free balance.
    TopUp,
    /// Excess collateral was returned to free balance.
    Release,
}

/// An audit record for a single collateral adjustment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollateralAdjustment {
    pub position_id: String,
    pub wallet: String,
    pub kind: AdjustmentKind,
    /// Signed amount moved: positive for top-ups, negative for releases.
    pub amount: f64,
    pub locked_before: f64,
    pub locked_after: f64,
    pub required_collateral: f64,
    pub spot: f64,
}

/// Events published on the private WS channel for a wallet.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemarginEvent {
    CollateralAdjusted {
        position_id: String,
        wallet: String,
        kind: AdjustmentKind,
        amount: f64,
        locked_collateral: f64,
    },
    MarginCall {
        wallet: String,
        position_id: String,
        shortfall: f64,
    },
}

/// Persistence and side-effect surface required by the re-margining process.
///
/// Implementations are expected to provide batched, per-wallet transactions
/// and compare-and-set guards so that positions closing mid-run are not
/// adjusted.
#[allow(async_fn_in_trait)]
pub trait RemarginStore {
    /// Fetch a batch of open short positions, oldest first.
    async fn open_short_positions(&self, limit: usize) -> Result<Vec<ShortPosition>, String>;

    /// Fetch the latest spot price for an underlying. `None` means the price is
    /// stale and the position must be skipped.
    async fn latest_spot(&self, position: &ShortPosition) -> Result<Option<f64>, String>;

    /// Fetch the wallet balance for a wallet.
    async fn wallet_balance(&self, wallet: &str) -> Result<Option<WalletBalance>, String>;

    /// Apply a collateral adjustment atomically. Implementations must use a
    /// compare-and-set guard on `expected_locked` and return `Ok(false)` if the
    /// position changed (e.g. closed) since it was read.
    async fn apply_adjustment(
        &self,
        position: &ShortPosition,
        expected_locked: f64,
        adjustment: &CollateralAdjustment,
    ) -> Result<bool, String>;

    /// Record an audit row for an adjustment.
    async fn record_adjustment(&self, adjustment: &CollateralAdjustment) -> Result<(), String>;

    /// Mark a wallet as being in margin call, hooking into the liquidation
    /// engine.
    async fn enter_margin_call(
        &self,
        wallet: &str,
        position_id: &str,
        shortfall: f64,
    ) -> Result<(), String>;

    /// Publish an event on the wallet's private WS channel.
    async fn publish_event(&self, wallet: &str, event: &RemarginEvent) -> Result<(), String>;

    /// Whether the portfolio margin engine is enabled for a wallet. When it is,
    /// this process must not run for that wallet.
    async fn portfolio_margin_enabled(&self, wallet: &str) -> Result<bool, String>;
}

/// Compute the collateral required for a short position at the given spot.
///
/// A short call's liability scales with spot, so required collateral is the
/// notional value of the short at the current spot.
pub fn required_collateral(position: &ShortPosition, spot: f64) -> f64 {
    position.quantity * spot
}

/// Decide whether a position should be re-margined and by how much.
///
/// Returns `None` when the drift is within the hysteresis band or below the
/// configured drift threshold, preventing flapping around the trigger point.
pub fn plan_adjustment(
    position: &ShortPosition,
    spot: f64,
    config: &RemarginConfig,
) -> Option<(AdjustmentKind, f64)> {
    let required = required_collateral(position, spot);
    let locked = position.locked_collateral;

    if locked <= 0.0 {
        return None;
    }

    let drift = (required - locked).abs() / locked;
    if drift < config.drift_threshold {
        return None;
    }

    // Hysteresis: only act when the required amount is outside the band around
    // the locked amount, so small oscillations do not cause repeated moves.
    let band = locked * config.hysteresis_band;
    if (required - locked).abs() <= band {
        return None;
    }

    if required > locked {
        Some((AdjustmentKind::TopUp, required - locked))
    } else {
        Some((AdjustmentKind::Release, locked - required))
    }
}

/// Run a single re-margining pass over open short positions.
///
/// Positions are grouped by wallet so that adjustments are applied in batched
/// per-wallet transactions. A stale price causes the position to be skipped,
/// and positions that close mid-run are guarded by compare-and-set in the
/// store. Wallets with the portfolio margin engine enabled are skipped
/// entirely, since that engine supersedes this logic.
pub async fn remargin_once<S: RemarginStore>(
    store: &S,
    config: &RemarginConfig,
) -> Result<Vec<CollateralAdjustment>, String> {
    let positions = store.open_short_positions(config.batch_size).await?;

    // Group by wallet to keep per-wallet transactions batched.
    let mut by_wallet: HashMap<String, Vec<ShortPosition>> = HashMap::new();
    for position in positions {
        if !position.is_open {
            continue;
        }
        by_wallet
            .entry(position.wallet.clone())
            .or_default()
            .push(position);
    }

    let mut applied = Vec::new();

    for (wallet, wallet_positions) in by_wallet {
        if store.portfolio_margin_enabled(&wallet).await? {
            // Portfolio margin supersedes this logic for this wallet.
            continue;
        }

        let mut balance = match store.wallet_balance(&wallet).await? {
            Some(balance) => balance,
            None => continue,
        };

        for position in wallet_positions {
            let spot = match store.latest_spot(&position).await? {
                Some(spot) => spot,
                // Stale price: skip this position.
                None => continue,
            };

            let (kind, amount) = match plan_adjustment(&position, spot, config) {
                Some(plan) => plan,
                None => continue,
            };

            let required = required_collateral(&position, spot);
            let locked_before = position.locked_collateral;

            match kind {
                AdjustmentKind::TopUp => {
                    if balance.free_balance < amount {
                        // Insufficient free balance: enter margin call.
                        let shortfall = amount - balance.free_balance;
                        store
                            .enter_margin_call(&wallet, &position.position_id, shortfall)
                            .await?;
                        store
                            .publish_event(
                                &wallet,
                                &RemarginEvent::MarginCall {
                                    wallet: wallet.clone(),
                                    position_id: position.position_id.clone(),
                                    shortfall,
                                },
                            )
                            .await?;
                        continue;
                    }

                    let adjustment = CollateralAdjustment {
                        position_id: position.position_id.clone(),
                        wallet: wallet.clone(),
                        kind,
                        amount,
                        locked_before,
                        locked_after: locked_before + amount,
                        required_collateral: required,
                        spot,
                    };

                    if !store
                        .apply_adjustment(&position, locked_before, &adjustment)
                        .await?
                    {
                        // Position changed (e.g. closed) mid-run; skip.
                        continue;
                    }

                    balance.free_balance -= amount;
                    balance.locked_collateral += amount;
                    store.record_adjustment(&adjustment).await?;
                    store
                        .publish_event(
                            &wallet,
                            &RemarginEvent::CollateralAdjusted {
                                position_id: position.position_id.clone(),
                                wallet: wallet.clone(),
                                kind,
                                amount,
                                locked_collateral: adjustment.locked_after,
                            },
                        )
                        .await?;
                    applied.push(adjustment);
                }
                AdjustmentKind::Release => {
                    let adjustment = CollateralAdjustment {
                        position_id: position.position_id.clone(),
                        wallet: wallet.clone(),
                        kind,
                        amount: -amount,
                        locked_before,
                        locked_after: locked_before - amount,
                        required_collateral: required,
                        spot,
                    };

                    if !store
                        .apply_adjustment(&position, locked_before, &adjustment)
                        .await?
                    {
                        continue;
                    }

                    balance.free_balance += amount;
                    balance.locked_collateral -= amount;
                    store.record_adjustment(&adjustment).await?;
                    store
                        .publish_event(
                            &wallet,
                            &RemarginEvent::CollateralAdjusted {
                                position_id: position.position_id.clone(),
                                wallet: wallet.clone(),
                                kind,
                                amount: -amount,
                                locked_collateral: adjustment.locked_after,
                            },
                        )
                        .await?;
                    applied.push(adjustment);
                }
            }
        }
    }

    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn position(locked: f64, entry_spot: f64, quantity: f64) -> ShortPosition {
        ShortPosition {
            position_id: "p1".into(),
            wallet: "w1".into(),
            locked_collateral: locked,
            entry_spot,
            quantity,
            is_open: true,
        }
    }

    #[test]
    fn spot_rise_triggers_top_up() {
        let pos = position(100.0, 100.0, 1.0);
        let config = RemarginConfig::default();
        let plan = plan_adjustment(&pos, 150.0, &config);
        assert_eq!(plan, Some((AdjustmentKind::TopUp, 50.0)));
    }

    #[test]
    fn spot_fall_triggers_release() {
        let pos = position(100.0, 100.0, 1.0);
        let config = RemarginConfig::default();
        let plan = plan_adjustment(&pos, 50.0, &config);
        assert_eq!(plan, Some((AdjustmentKind::Release, 50.0)));
    }

    #[test]
    fn hysteresis_prevents_flapping() {
        let pos = position(100.0, 100.0, 1.0);
        let config = RemarginConfig::default();
        // 5% drift is below the 10% threshold.
        assert_eq!(plan_adjustment(&pos, 105.0, &config), None);
        // 11% drift is above the threshold but inside the 2% band? No: band is
        // 2% of locked, so 11% is outside and triggers.
        assert!(plan_adjustment(&pos, 111.0, &config).is_some());
    }

    struct MockStore {
        positions: Vec<ShortPosition>,
        spot: Option<f64>,
        balance: WalletBalance,
        portfolio_margin: bool,
        adjustments: RefCell<Vec<CollateralAdjustment>>,
        margin_calls: RefCell<Vec<(String, f64)>>,
        events: RefCell<Vec<RemarginEvent>>,
    }

    impl RemarginStore for MockStore {
        async fn open_short_positions(
            &self,
            limit: usize,
        ) -> Result<Vec<ShortPosition>, String> {
            Ok(self.positions.iter().take(limit).cloned().collect())
        }

        async fn latest_spot(&self, _p: &ShortPosition) -> Result<Option<f64>, String> {
            Ok(self.spot)
        }

        async fn wallet_balance(&self, _w: &str) -> Result<Option<WalletBalance>, String> {
            Ok(Some(self.balance.clone()))
        }

        async fn apply_adjustment(
            &self,
            _p: &ShortPosition,
            _expected: f64,
            adjustment: &CollateralAdjustment,
        ) -> Result<bool, String> {
            self.adjustments.borrow_mut().push(adjustment.clone());
            Ok(true)
        }

        async fn record_adjustment(&self, _a: &CollateralAdjustment) -> Result<(), String> {
            Ok(())
        }

        async fn enter_margin_call(
            &self,
            _w: &str,
            _p: &str,
            shortfall: f64,
        ) -> Result<(), String> {
            self.margin_calls.borrow_mut().push(("w1".into(), shortfall));
            Ok(())
        }

        async fn publish_event(&self, _w: &str, event: &RemarginEvent) -> Result<(), String> {
            self.events.borrow_mut().push(event.clone());
            Ok(())
        }

        async fn portfolio_margin_enabled(&self, _w: &str) -> Result<bool, String> {
            Ok(self.portfolio_margin)
        }
    }

    fn mock(spot: Option<f64>, free: f64, portfolio_margin: bool) -> MockStore {
        MockStore {
            positions: vec![position(100.0, 100.0, 1.0)],
            spot,
            balance: WalletBalance {
                wallet: "w1".into(),
                free_balance: free,
                locked_collateral: 100.0,
            },
            portfolio_margin,
            adjustments: RefCell::new(Vec::new()),
            margin_calls: RefCell::new(Vec::new()),
            events: RefCell::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn top_up_draws_from_free_balance() {
        let store = mock(Some(150.0), 100.0, false);
        let applied = remargin_once(&store, &RemarginConfig::default()).await.unwrap();
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].kind, AdjustmentKind::TopUp);
        assert_eq!(applied[0].amount, 50.0);
        assert!(store.margin_calls.borrow().is_empty());
    }

    #[test]
    fn insufficient_funds_enters_margin_call() {
        let store = mock(Some(150.0), 10.0, false);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let applied = rt
            .block_on(remargin_once(&store, &RemarginConfig::default()))
            .unwrap();
        assert!(applied.is_empty());
        assert_eq!(store.margin_calls.borrow().len(), 1);
        assert_eq!(store.margin_calls.borrow()[0].1, 40.0);
    }

    #[test]
    fn stale_price_is_skipped() {
        let store = mock(None, 100.0, false);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let applied = rt
            .block_on(remargin_once(&store, &RemarginConfig::default()))
            .unwrap();
        assert!(applied.is_empty());
    }

    #[test]
    fn portfolio_margin_supersedes() {
        let store = mock(Some(150.0), 100.0, true);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let applied = rt
            .block_on(remargin_once(&store, &RemarginConfig::default()))
            .unwrap();
        assert!(applied.is_empty());
    }
}
