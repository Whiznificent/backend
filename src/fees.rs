//! Trading fee model with tiered schedules and fee accrual.
//!
//! Fees are charged on each trade as `min(bps * underlying_notional, cap_pct * premium)`,
//! the industry-standard options fee cap. Volume-based tiers are recalculated daily from
//! 30d volume, maker/taker distinctions apply to limit orders, and every fee accrues to the
//! `protocol_fees` ledger account.
//!
//! The core computation is a pure function [`fee`] so it can be golden-tested at every tier
//! and cap boundary. Fees are rounded against the user (up) with a minimum-fee floor to
//! prevent dust splitting exploits.

use rust_decimal::prelude::*;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Ledger account that all accrued fees are recorded against.
pub const PROTOCOL_FEES_ACCOUNT: &str = "protocol_fees";

/// The kind of execution a fee is being charged for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeKind {
    Open,
    Close,
    /// A roll is two legs; each leg is charged independently.
    Roll,
    /// Optional, separately configured settlement fee.
    Settlement,
}

/// Whether the order that produced the trade was a maker or taker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liquidity {
    Maker,
    Taker,
}

/// A single trade leg to be charged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub kind: TradeKind,
    pub liquidity: Liquidity,
    /// Premium of the option leg (per contract, in quote currency).
    pub premium: Decimal,
    /// Number of contracts traded.
    pub quantity: Decimal,
    /// Underlying notional per contract (e.g. index price * contract multiplier).
    pub underlying_notional: Decimal,
}

impl Trade {
    /// Total premium paid for this leg.
    pub fn total_premium(&self) -> Decimal {
        self.premium * self.quantity
    }

    /// Total underlying notional for this leg.
    pub fn total_notional(&self) -> Decimal {
        self.underlying_notional * self.quantity
    }
}

/// A versioned, effective-dated fee schedule.
///
/// The schedule effective at execution time is the one with the greatest
/// `effective_from` that is `<= execution_time`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeSchedule {
    pub version: i64,
    /// Unix timestamp (seconds) from which this schedule applies.
    pub effective_from: i64,
    /// Base fee in basis points of underlying notional.
    pub maker_bps: Decimal,
    pub taker_bps: Decimal,
    /// Cap as a fraction of premium (e.g. 0.125 = 12.5%).
    pub cap_pct: Decimal,
    /// Minimum fee floor, to prevent dust splitting.
    pub min_fee: Decimal,
    /// Optional separate settlement fee (flat, per settlement).
    pub settlement_fee: Option<Decimal>,
}

impl FeeSchedule {
    /// Basis points applicable for the given liquidity side.
    pub fn bps(&self, liquidity: Liquidity) -> Decimal {
        match liquidity {
            Liquidity::Maker => self.maker_bps,
            Liquidity::Taker => self.taker_bps,
        }
    }
}

/// A wallet's volume tier, recalculated daily from trailing 30d volume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletFeeTier {
    pub wallet: String,
    pub tier: i32,
    /// Multiplier applied to the schedule bps (e.g. 0.8 = 20% discount).
    pub bps_multiplier: Decimal,
    /// Trailing 30d volume used to derive the tier.
    pub volume_30d: Decimal,
    /// Unix timestamp of the last daily recalculation.
    pub recalculated_at: i64,
}

impl WalletFeeTier {
    /// The default (highest-fee) tier for wallets with no volume history.
    pub fn base(wallet: impl Into<String>) -> Self {
        Self {
            wallet: wallet.into(),
            tier: 0,
            bps_multiplier: Decimal::ONE,
            volume_30d: Decimal::ZERO,
            recalculated_at: 0,
        }
    }
}

/// Select the schedule effective at `execution_time`.
///
/// Returns `None` if no schedule is yet effective. Schedule changes that take
/// effect mid-day are handled naturally: the schedule effective at execution
/// time is used.
pub fn effective_schedule(schedules: &[FeeSchedule], execution_time: i64) -> Option<&FeeSchedule> {
    schedules
        .iter()
        .filter(|s| s.effective_from <= execution_time)
        .max_by_key(|s| s.effective_from)
}

/// Compute the fee for a single trade leg.
///
/// `fee = min(bps * notional, cap_pct * premium)`, then floored at `min_fee`
/// and rounded up (against the user). Zero-premium trades still pay the floor.
pub fn fee(trade: &Trade, schedule: &FeeSchedule, tier: &WalletFeeTier) -> Decimal {
    // Settlement is a separate, optional flat fee.
    if trade.kind == TradeKind::Settlement {
        return schedule.settlement_fee.unwrap_or(Decimal::ZERO);
    }

    let bps = schedule.bps(trade.liquidity) * tier.bps_multiplier;
    let notional_fee = bps / Decimal::from(10_000) * trade.total_notional();
    let cap_fee = schedule.cap_pct * trade.total_premium();

    // The cap protects deep-OTM options from being overcharged.
    let mut charged = notional_fee.min(cap_fee);

    // Minimum-fee floor prevents dust splitting.
    if charged < schedule.min_fee {
        charged = schedule.min_fee;
    }

    // Round against the user (up) to the smallest quote unit.
    round_up(charged, Decimal::new(1, 8))
}

/// Round `value` up to the nearest multiple of `step` (against the user).
fn round_up(value: Decimal, step: Decimal) -> Decimal {
    if step.is_zero() {
        return value;
    }
    let units = (value / step).ceil();
    units * step
}

/// A single accrued fee entry destined for the ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeeAccrual {
    pub account: &'static str,
    pub amount: Decimal,
    pub schedule_version: i64,
    pub tier: i32,
    pub kind: TradeKind,
}

/// Charge fees for a trade and produce the ledger accruals.
///
/// A roll produces two accruals (one per leg). Settlement produces a single
/// optional accrual. Every accrual is recorded against [`PROTOCOL_FEES_ACCOUNT`].
pub fn accrue(
    trades: &[Trade],
    schedule: &FeeSchedule,
    tier: &WalletFeeTier,
) -> Vec<FeeAccrual> {
    trades
        .iter()
        .filter_map(|trade| {
            let amount = fee(trade, schedule, tier);
            if amount.is_zero() {
                return None;
            }
            Some(FeeAccrual {
                account: PROTOCOL_FEES_ACCOUNT,
                amount,
                schedule_version: schedule.version,
                tier: tier.tier,
                kind: trade.kind,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule() -> FeeSchedule {
        FeeSchedule {
            version: 1,
            effective_from: 0,
            maker_bps: Decimal::new(10, 2),  // 0.10 bps
            taker_bps: Decimal::new(30, 2),  // 0.30 bps
            cap_pct: Decimal::new(125, 3),   // 12.5%
            min_fee: Decimal::new(1, 2),     // 0.01
            settlement_fee: Some(Decimal::new(5, 1)),
        }
    }

    fn trade(premium: i64, notional: i64) -> Trade {
        Trade {
            kind: TradeKind::Open,
            liquidity: Liquidity::Taker,
            premium: Decimal::from(premium),
            quantity: Decimal::ONE,
            underlying_notional: Decimal::from(notional),
        }
    }

    #[test]
    fn cap_protects_deep_otm() {
        let s = schedule();
        let t = WalletFeeTier::base("w");
        // notional fee = 0.30bps * 100000 = 3.0; cap = 12.5% * 1 = 0.125
        let f = fee(&trade(1, 100_000), &s, &t);
        assert_eq!(f, Decimal::new(125, 3));
    }

    #[test]
    fn min_fee_floor_prevents_dust() {
        let s = schedule();
        let t = WalletFeeTier::base("w");
        let f = fee(&trade(1, 1), &s, &t);
        assert_eq!(f, s.min_fee);
    }

    #[test]
    fn settlement_uses_flat_fee() {
        let s = schedule();
        let t = WalletFeeTier::base("w");
        let mut tr = trade(10, 1000);
        tr.kind = TradeKind::Settlement;
        assert_eq!(fee(&tr, &s, &t), Decimal::new(5, 1));
    }

    #[test]
    fn effective_schedule_picks_latest() {
        let mut newer = schedule();
        newer.version = 2;
        newer.effective_from = 100;
        let schedules = vec![schedule(), newer];
        assert_eq!(effective_schedule(&schedules, 150).unwrap().version, 2);
        assert_eq!(effective_schedule(&schedules, 50).unwrap().version, 1);
    }

    #[test]
    fn roll_accrues_both_legs() {
        let s = schedule();
        let t = WalletFeeTier::base("w");
        let mut a = trade(10, 1000);
        a.kind = TradeKind::Roll;
        let mut b = trade(10, 1000);
        b.kind = TradeKind::Roll;
        let accruals = accrue(&[a, b], &s, &t);
        assert_eq!(accruals.len(), 2);
        assert!(accruals.iter().all(|a| a.account == PROTOCOL_FEES_ACCOUNT));
    }
}
