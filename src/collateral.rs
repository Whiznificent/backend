//! Collateral requirements for writing (selling) options, ported bit-for-bit
//! from the frontend's `lib/collateral.ts`: covered calls are 100% covered
//! by the underlying's current value, cash-secured puts are
//! over-collateralized by 110% of the strike (protects against a further
//! drop before the writer can react). Only applies to the short/write
//! side — buying an option never requires collateral, just the premium.
//!
//! Collateral requirements for writing (selling) options, ported bit-for-bit
//! from the frontend's `lib/collateral.ts`: covered calls are 100% covered
//! by the underlying's current value, cash-secured puts are
//! over-collateralized by 110% of the strike (protects against a further
//! drop before the writer can react). Only applies to the short/write
//! side — buying an option never requires collateral, just the premium.
//!
//! Monetary results are returned as fixed-point [`Money`] (a `rust_decimal`
//! newtype) rather than `f64`. The pricing inputs (`strike`, `spot`) stay
//! `f64` because the pricing math is `f64` internally; the conversion
//! boundary is explicit via [`Money::from_price`], which rejects NaN/Inf.
//! Collateral is rounded *against the user* (up, away from zero) so a writer
//! is never under-collateralized by a rounding artifact.
//!
//! Dynamic re-margining (issue #41): a short position's collateral is fixed
//! at open, so a spot move silently under- or over-collateralises it. The
//! helpers below recompute the required collateral and decide whether a
//! top-up (draw from free balance) or a release (return excess to free
//! balance) is warranted, using hysteresis bands so the account does not
//! flap around the trigger threshold.
//!
//! Issue #24 introduces a portfolio margin engine on top of these legacy
//! per-leg rules. The legacy rules are preserved verbatim as
//! [`StrategyBasedMargin`] (the fallback), while [`RiskArrayMargin`]
//! computes a wallet's requirement as the worst-case loss across a stress
//! grid of spot shocks × vol shocks (SPAN-style), so hedged, defined-risk
//! strategies such as spreads, iron condors and butterflies only lock their
//! true maximum loss. Both implement the [`MarginModel`] trait so callers
//! (open/close/roll/strategy execution) can select a model per environment
//! via a feature flag.

use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;

/// Fixed-point monetary amount backed by `rust_decimal::Decimal`.
///
/// Display uses banker's rounding; fees and collateral use
/// round-against-the-user (see [`Money::round_against_user`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Money(Decimal);

impl Money {
    /// Explicit conversion boundary from the `f64` pricing math.
    ///
    /// Returns `None` for NaN and ±Inf so callers can surface

/// A single option leg held by a wallet, as seen by the margin engine.
///
/// `contracts` is signed: positive for long, negative for short. `strike`
/// and `spot` are in the same quote currency. `vol` is the implied
/// volatility used to reprice the leg under a vol shock.
#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub option_type: String,
    pub contracts: f64,
    pub strike: f64,
    pub spot: f64,
    pub vol: f64,
}

/// The worst scenario found on the stress grid, reported by
/// `GET /api/v1/account/margin` alongside the requirement.
#[derive(Debug, Clone, PartialEq)]
pub struct WorstScenario {
    pub spot_shock: f64,
    pub vol_shock: f64,
    pub loss: f64,
}

/// A wallet's margin requirement, broken down for the margin endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct MarginRequirement {
    /// Initial requirement: the worst-case loss across the stress grid.
    pub initial: f64,
    /// Maintenance requirement: a fraction of the initial requirement.
    pub maintenance: f64,
    /// The scenario that produced the initial requirement.
    pub worst_scenario: WorstScenario,
    /// Per-position contribution to the initial requirement.
    pub contributions: Vec<f64>,
}

/// Fraction of the initial requirement that must be maintained.
const MAINTENANCE_FRACTION: f64 = 0.75;

/// Spot shocks applied to every leg, as fractions of the current spot.
/// The top shock is large enough (+100%) to bound the otherwise unbounded
/// upside of a naked short call.
const SPOT_SHOCKS: [f64; 7] = [-0.30, -0.15, -0.05, 0.0, 0.05, 0.15, 1.0];

/// Volatility shocks applied to every leg, as absolute vol points.
const VOL_SHOCKS: [f64; 3] = [-0.20, 0.0, 0.20];

/// A margin model computes a wallet's requirement from its post-trade
/// position set. Implementations must be pure so the what-if check and the
/// commit see the same numbers inside one transaction.
pub trait MarginModel {
    /// Compute the requirement for the given post-trade positions.
    fn requirement(&self, positions: &[Position]) -> MarginRequirement;

    /// Whether the post-trade initial margin would exceed `equity`.
    /// Used to reject a trade with 422 before commit.
    fn exceeds_equity(&self, positions: &[Position], equity: f64) -> bool {
        self.requirement(positions).initial > equity
    }
}

/// Legacy per-leg rules, kept as the fallback model. Short calls lock 100%
/// of spot, short puts lock 110% of strike; long legs require nothing.
pub struct StrategyBasedMargin;

impl MarginModel for StrategyBasedMargin {
    fn requirement(&self, positions: &[Position]) -> MarginRequirement {
        let mut contributions = Vec::with_capacity(positions.len());
        let mut initial = 0.0;
        for p in positions {
            let c = if p.contracts < 0.0 {
                collateral_required(&p.option_type, -p.contracts, p.strike, p.spot)
            } else {
                0.0
            };
            initial += c;
            contributions.push(c);
        }
        MarginRequirement {
            initial,
            maintenance: initial * MAINTENANCE_FRACTION,
            worst_scenario: WorstScenario {
                spot_shock: 0.0,
                vol_shock: 0.0,
                loss: initial,
            },
            contributions,
        }
    }
}

/// SPAN-style stress grid: reprices every leg under each spot × vol shock
/// and takes the worst-case loss as the requirement. Hedged, defined-risk
/// strategies therefore only lock their true maximum loss.
pub struct RiskArrayMargin;

impl MarginModel for RiskArrayMargin {
    fn requirement(&self, positions: &[Position]) -> MarginRequirement {
        let mut worst = WorstScenario {
            spot_shock: 0.0,
            vol_shock: 0.0,
            loss: 0.0,
        };
        for &spot_shock in &SPOT_SHOCKS {
            for &vol_shock in &VOL_SHOCKS {
                let loss: f64 = positions
                    .iter()
                    .map(|p| leg_loss(p, spot_shock, vol_shock))
                    .sum();
                if loss > worst.loss {
                    worst = WorstScenario {
                        spot_shock,
                        vol_shock,
                        loss,
                    };
                }
            }
        }
        let initial = worst.loss.max(0.0);
        let contributions = positions
            .iter()
            .map(|p| leg_loss(p, worst.spot_shock, worst.vol_shock).max(0.0))
            .collect();
        MarginRequirement {
            initial,
            maintenance: initial * MAINTENANCE_FRACTION,
            worst_scenario: worst,
            contributions,
        }
    }
}

/// Loss of a single leg under a spot and vol shock. Long legs lose when the
/// option loses value; short legs lose when it gains value.
fn leg_loss(p: &Position, spot_shock: f64, vol_shock: f64) -> f64 {
    let shocked_spot = p.spot * (1.0 + spot_shock);
    let shocked_vol = (p.vol + vol_shock).max(0.0);
    let base = option_value(&p.option_type, p.spot, p.strike, p.vol);
    let shocked = option_value(&p.option_type, shocked_spot, p.strike, shocked_vol);
    // A short position (negative contracts) loses when the option value rises.
    -p.contracts * (shocked - base)
}

/// Intrinsic-plus-time option value used for repricing. Kept deliberately
/// simple and deterministic so the grid is reproducible; the time value
/// scales with vol and decays as the option moves out of the money.
fn option_value(option_type: &str, spot: f64, strike: f64, vol: f64) -> f64 {
    let intrinsic = if option_type == "call" {
        (spot - strike).max(0.0)
    } else {
        (strike - spot).max(0.0)
    };
    let moneyness = (spot - strike).abs() / strike.max(f64::EPSILON);
    let time_value = spot * vol * (1.0 - moneyness).max(0.0);
    intrinsic + time_value
}

use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;

/// Fixed-point monetary amount backed by `rust_decimal::Decimal`.
///
/// Display uses banker's rounding; fees and collateral use
/// round-against-the-user (see [`Money::round_against_user`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Money(Decimal);

impl Money {
    /// Explicit conversion boundary from the `f64` pricing math.
    ///
    /// Returns `None` for NaN and ±Inf so callers can surface

use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;

/// Fixed-point monetary amount backed by `rust_decimal::Decimal`.
///
/// Display uses banker's rounding; fees and collateral use
/// round-against-the-user (see [`Money::round_against_user`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Money(Decimal);

impl Money {
    /// Explicit conversion boundary from the `f64` pricing math.
    ///
    /// Returns `None` for NaN and ±Inf so callers can surface an error
    /// instead of silently propagating a non-finite balance.
    pub fn from_price(value: f64) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }
        Decimal::from_f64_retain(value).map(Money)
    }

    /// Round-against-the-user: round away from zero to `scale` decimal
    /// places. Used for fees and collateral so the protocol never loses.
    pub fn round_against_user(self, scale: u32) -> Self {
        Money(self.0.round_dp_with_strategy(
            scale,
            RoundingStrategy::AwayFromZero,
        ))
    }

    /// Banker's rounding (round-half-to-even) for display purposes.
    pub fn round_for_display(self, scale: u32) -> Self {
        Money(self.0.round_dp_with_strategy(
            scale,
            RoundingStrategy::ToEven,
        ))
    }

    /// Checked multiplication; returns `None` on overflow instead of panicking.
    pub fn checked_mul(self, rhs: Self) -> Option<Self> {
        self.0.checked_mul(rhs.0).map(Money)
    }

    /// Checked addition; returns `None` on overflow instead of panicking.
    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        self.0.checked_add(rhs.0).map(Money)
    }

    /// The underlying decimal value.
    pub fn value(self) -> Decimal {
        self.0
    }
}

impl std::fmt::Display for Money {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Banker's rounding for display, then render as a plain string so
        // JSON consumers never see a JS float precision artifact.
        write!(f, "{}", self.round_for_display(7).0)
    }
}

/// Collateral required to write an option, as fixed-point [`Money`].
///
/// Covered calls are 100% of the underlying's current value; cash-secured
/// puts are 110% of the strike. Returns `None` if any input is non-finite
/// or if the multiplication overflows.
pub fn collateral_required(
    option_type: &str,
    contracts: f64,
    strike: f64,
    spot: f64,
) -> Option<Money> {
    let contracts = Money::from_price(contracts)?;
    if option_type == "call" {
        let spot = Money::from_price(spot)?;
        contracts.checked_mul(spot)
    } else {
        let strike = Money::from_price(strike)?;
        // 110% over-collateralization, expressed exactly as a decimal.
        let buffer = Money::from_price(1.1)?;
        contracts.checked_mul(strike)?.checked_mul(buffer)
    }
    .map(|m| m.round_against_user(7))
}

/// Configuration for the re-margining loop. `drift_threshold` is the
/// configurable X% (as a fraction, e.g. 0.05 for 5%) that required
/// collateral must drift from the locked amount before a re-margin is
/// triggered. `hysteresis` is the fraction of the locked amount that must
/// be recovered before a release is allowed, preventing flapping.
pub struct RemarginConfig {
    pub drift_threshold: f64,
    pub hysteresis: f64,
}

impl Default for RemarginConfig {
    fn default() -> Self {
        Self {
            drift_threshold: 0.05,
            hysteresis: 0.02,
        }
    }
}

/// The action the re-margining loop should take for a single short position.
#[derive(Debug, PartialEq)]
pub enum RemarginAction {
    /// No drift beyond the threshold; leave collateral untouched.
    None,
    /// Required collateral rose; draw `amount` from free balance.
    TopUp { amount: f64 },
    /// Required collateral fell; return `amount` to free balance.
    Release { amount: f64 },
}

/// Decide whether an open short position needs re-margining given the
/// current spot price. `locked` is the collateral currently held for the
/// position and `free_balance` is the wallet's available balance.
///
/// Returns the action to apply. A top-up larger than the free balance is
/// still returned as `TopUp`; the caller is responsible for entering
/// `margin_call` when the free balance is insufficient (hooked into the
/// liquidation engine).
pub fn remargin_action(
    option_type: &str,
    contracts: f64,
    strike: f64,
    spot: f64,
    locked: f64,
    config: &RemarginConfig,
) -> RemarginAction {
    let required = collateral_required(option_type, contracts, strike, spot);
    let drift = required - locked;

    if locked <= 0.0 {
        return RemarginAction::None;
    }

    let threshold = locked * config.drift_threshold;

    if drift > threshold {
        RemarginAction::TopUp { amount: drift }
    } else if -drift > threshold + locked * config.hysteresis {
        RemarginAction::Release { amount: -drift }
    } else {
        RemarginAction::None
    }
}

/// Returns `(wallet_address, collateral_locked, open_collateral_sum)` for every
/// wallet whose `collateral_locked` has drifted from the sum of its open
/// positions' collateral. An empty result means every wallet's locked
/// collateral is fully accounted for by its open positions.
///
/// This is a defence-in-depth check job, not a request-path query: the
/// application maintains the sum in the same transaction as every open/close/
/// roll, so a non-empty result means something outside that path (a manual SQL
/// fix, a future service writing to the same database) corrupted the
/// invariant. It uses a runtime query rather than a checked macro because it
/// is a diagnostic that runs on demand, not on the request path.
pub async fn check_collateral_sum(
    db: &sqlx::SqlitePool,
) -> Result<Vec<(String, f64, f64)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT a.wallet_address, a.collateral_locked, COALESCE(SUM(p.collateral), 0.0)
           FROM accounts a
           LEFT JOIN positions p ON p.wallet_address = a.wallet_address AND p.status = 'open'
          GROUP BY a.wallet_address, a.collateral_locked
         HAVING a.collateral_locked != COALESCE(SUM(p.collateral), 0.0)",
    )
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn covered_call_is_100_percent_of_spot() {
        let got = collateral_required("call", 2.0, 70000.0, 67420.50).unwrap();
        let want = Money::from_price(2.0 * 67420.50).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn cash_secured_put_is_110_percent_of_strike() {
        let got = collateral_required("put", 3.0, 60000.0, 67420.50).unwrap();
        let want = Money::from_price(3.0 * 60000.0 * 1.1).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn rejects_nan_and_inf() {
        assert!(collateral_required("call", f64::NAN, 70000.0, 67420.50).is_none());
        assert!(collateral_required("call", 2.0, 70000.0, f64::INFINITY).is_none());
        assert!(Money::from_price(f64::NEG_INFINITY).is_none());
    }

    #[test]
    fn collateral_rounds_against_the_user() {
        // 1 contract * 0.00000001 strike * 1.1 = 0.000000011 -> rounds up to 1e-7.
        let got = collateral_required("put", 1.0, 0.00000001, 1.0).unwrap();
        assert_eq!(got.value(), rust_decimal::dec!(0.0000001));
    }

    #[test]
    fn spot_rise_triggers_top_up() {
        let cfg = RemarginConfig::default();
        // 1 call locked at spot 100 -> 100. Spot doubles to 200.
        let action = remargin_action("call", 1.0, 0.0, 200.0, 100.0, &cfg);
        assert_eq!(action, RemarginAction::TopUp { amount: 100.0 });
    }

    #[test]
    fn spot_fall_triggers_release() {
        let cfg = RemarginConfig::default();
        // 1 call locked at spot 200 -> 200. Spot falls to 100.
        let action = remargin_action("call", 1.0, 0.0, 100.0, 200.0, &cfg);
        assert_eq!(action, RemarginAction::Release { amount: 100.0 });
    }

    #[test]
    fn hysteresis_prevents_flapping() {
        let cfg = RemarginConfig::default();
        // Required 104 vs locked 100: 4% drift, below the 5% threshold.
        assert_eq!(
            remargin_action("call", 1.0, 0.0, 104.0, 100.0, &cfg),
            RemarginAction::None
        );
        // Required 96 vs locked 100: 4% drift, below threshold + hysteresis.
        assert_eq!(
            remargin_action("call", 1.0, 0.0, 96.0, 100.0, &cfg),
            RemarginAction::None
        );
    }

    #[tokio::test]
    async fn check_collateral_sum_flags_a_drifted_wallet() {
        let db_path =
            std::env::temp_dir().join(format!("zenith-collateral-test-{}.db", uuid::Uuid::new_v4()));
        let pool = crate::db::init_pool(&format!("sqlite://{}", db_path.display())).await;

        sqlx::query("INSERT INTO accounts (wallet_address, collateral_locked) VALUES ('GTEST', 100)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO positions
                (id, wallet_address, underlying, strike, expiry_days, option_type,
                 position_type, contracts, entry_premium, entry_spot, collateral, status)
             VALUES ('p1', 'GTEST', 'BTC', 70000, 30, 'call', 'short', 1, 100, 67000, 100, 'open')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Locked collateral (100) matches the open position's collateral (100).
        let drift = check_collateral_sum(&pool).await.unwrap();
        assert!(drift.is_empty(), "no drift expected: {drift:?}");

        // Simulate corruption outside the app's transactional open/close path.
        sqlx::query("UPDATE accounts SET collateral_locked = 50 WHERE wallet_address = 'GTEST'")
            .execute(&pool)
            .await
            .unwrap();

        let drift = check_collateral_sum(&pool).await.unwrap();
        assert_eq!(drift.len(), 1, "the drifted wallet must be reported");
        let (wallet, locked, sum) = &drift[0];
        assert_eq!(wallet, "GTEST");
        assert_eq!(*locked, 50.0);
        assert_eq!(*sum, 100.0);

        pool.close().await;
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn strategy_based_matches_legacy_rules() {
        let positions = vec![
            Position {
                option_type: "call".into(),
                contracts: -2.0,
                strike: 70000.0,
                spot: 67420.50,
                vol: 0.6,
            },
            Position {
                option_type: "put".into(),
                contracts: -3.0,
                strike: 60000.0,
                spot: 67420.50,
                vol: 0.6,
            },
        ];
        let req = StrategyBasedMargin.requirement(&positions);
        assert_eq!(req.initial, 2.0 * 67420.50 + 3.0 * 60000.0 * 1.1);
        assert_eq!(req.maintenance, req.initial * MAINTENANCE_FRACTION);
    }

    #[test]
    fn long_legs_require_no_collateral() {
        let positions = vec![Position {
            option_type: "call".into(),
            contracts: 1.0,
            strike: 70000.0,
            spot: 67420.50,
            vol: 0.6,
        }];
        assert_eq!(StrategyBasedMargin.requirement(&positions).initial, 0.0);
    }

    #[test]
    fn vertical_spread_locks_width_minus_credit() {
        // 1-wide bull put spread: short 60000 put, long 59000 put.
        let positions = vec![
            Position {
                option_type: "put".into(),
                contracts: -1.0,
                strike: 60000.0,
                spot: 60000.0,
                vol: 0.0,
            },
            Position {
                option_type: "put".into(),
                contracts: 1.0,
                strike: 59000.0,
                spot: 60000.0,
                vol: 0.0,
            },
        ];
        let req = RiskArrayMargin.requirement(&positions);
        // Worst case is the full width (1000) at the deepest down shock.
        assert!(req.initial <= 1000.0 + 1e-9);
        assert!(req.initial > 0.0);
    }

    #[test]
    fn naked_short_call_is_bounded_by_top_shock() {
        let positions = vec![Position {
            option_type: "call".into(),
            contracts: -1.0,
            strike: 70000.0,
            spot: 67420.50,
            vol: 0.0,
        }];
        let req = RiskArrayMargin.requirement(&positions);
        // +100% spot shock bounds the unbounded upside.
        assert!(req.initial > 0.0);
        assert_eq!(req.worst_scenario.spot_shock, 1.0);
    }

    #[test]
    fn exceeds_equity_rejects_under_margined_trade() {
        let positions = vec![Position {
            option_type: "call".into(),
            contracts: -1.0,
            strike: 70000.0,
            spot: 67420.50,
            vol: 0.0,
        }];
        assert!(RiskArrayMargin.exceeds_equity(&positions, 1.0));
        assert!(!RiskArrayMargin.exceeds_equity(&positions, 1e12));
    }

    }
}
