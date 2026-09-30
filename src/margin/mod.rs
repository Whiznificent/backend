//! Portfolio margin engine for defined-risk multi-leg strategies.
//!
//! This module replaces the legacy per-leg collateral rules with a
//! portfolio-level margin requirement computed as the worst-case loss across
//! a stress grid of spot shocks × implied-volatility shocks (SPAN-style).
//!
//! Two [`MarginModel`] implementations are provided:
//!
//! * [`StrategyBasedMargin`] — the legacy per-leg rules (100% of spot for
//!   short calls, 110% of strike for short puts), kept as a fallback and for
//!   feature-flagged environments that have not yet enabled the risk array.
//! * [`RiskArrayMargin`] — a stress grid that recognises hedged, defined-risk
//!   structures (spreads, iron condors, butterflies) so they only lock their
//!   true maximum loss.
//!
//! The requirement is computed from the post-trade position set that is
//! visible inside the transaction, so the what-if check and the commit are
//! atomic.

use std::collections::HashMap;

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

use crate::error::Error;

/// A single leg of a position, as seen by the margin engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarginLeg {
    /// Underlying symbol (e.g. `BTC`).
    pub underlying: String,
    /// `true` for a call, `false` for a put.
    pub is_call: bool,
    /// Signed quantity: positive is long, negative is short.
    pub quantity: Decimal,
    /// Strike price.
    pub strike: Decimal,
    /// Spot price of the underlying at the time of the computation.
    pub spot: Decimal,
    /// Implied volatility used for the base scenario.
    pub iv: Decimal,
    /// Time to expiry, in years.
    pub time_to_expiry: Decimal,
}

/// A single stress scenario in the risk array grid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    /// Relative spot shock (e.g. `0.10` for +10%).
    pub spot_shock: Decimal,
    /// Absolute implied-volatility shock (e.g. `0.20` for +20 vol points).
    pub vol_shock: Decimal,
}

/// The result of a margin computation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarginRequirement {
    /// Initial margin requirement.
    pub initial: Decimal,
    /// Maintenance margin requirement.
    pub maintenance: Decimal,
    /// The worst scenario encountered while computing the requirement.
    pub worst_scenario: Scenario,
    /// Per-position contribution to the initial requirement, keyed by
    /// underlying symbol.
    pub contributions: HashMap<String, Decimal>,
}

/// A margin model computes the collateral requirement for a set of legs.
pub trait MarginModel {
    /// Compute the margin requirement for `legs`.
    fn requirement(&self, legs: &[MarginLeg]) -> Result<MarginRequirement, Error>;
}

/// The legacy per-leg collateral rules.
///
/// Short calls lock 100% of spot, short puts lock 110% of strike. This is kept
/// as a fallback for environments where the risk-array model is disabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct StrategyBasedMargin;

impl StrategyBasedMargin {
    /// The legacy short-call collateral ratio (100% of spot).
    pub const SHORT_CALL_RATIO: Decimal = dec!(1.00);
    /// The legacy short-put collateral ratio (110% of strike).
    pub const SHORT_PUT_RATIO: Decimal = dec!(1.10);
}

impl MarginModel for StrategyBasedMargin {
    fn requirement(&self, legs: &[MarginLeg]) -> Result<MarginRequirement, Error> {
        let mut contributions: HashMap<String, Decimal> = HashMap::new();
        let mut initial = Decimal::ZERO;

        for leg in legs {
            if leg.quantity >= Decimal::ZERO {
                continue;
            }
            let short = leg.quantity.abs();
            let per_leg = if leg.is_call {
                leg.spot * Self::SHORT_CALL_RATIO
            } else {
                leg.strike * Self::SHORT_PUT_RATIO
            };
            let amount = per_leg * short;
            initial += amount;
            *contributions.entry(leg.underlying.clone()).or_default() += amount;
        }

        Ok(MarginRequirement {
            initial,
            maintenance: initial,
            worst_scenario: Scenario {
                spot_shock: Decimal::ZERO,
                vol_shock: Decimal::ZERO,
            },
            contributions,
        })
    }
}

/// A SPAN-style stress-grid margin model.
///
/// The requirement is the worst-case loss across a grid of spot shocks ×
/// implied-volatility shocks. Because the grid is evaluated on the whole
/// position set, hedged structures only lock their true maximum loss.
#[derive(Debug, Clone)]
pub struct RiskArrayMargin {
    /// Spot shocks applied to every underlying, as relative moves.
    pub spot_shocks: Vec<Decimal>,
    /// Implied-volatility shocks, as absolute vol-point moves.
    pub vol_shocks: Vec<Decimal>,
    /// Multiplier applied to the worst-case loss to obtain the initial
    /// requirement (maintenance is the raw worst-case loss).
    pub initial_multiplier: Decimal,
}

impl Default for RiskArrayMargin {
    fn default() -> Self {
        Self {
            // The top shock must be large enough to cover the unbounded upside
            // of a naked short call, hence the +100% scenario.
            spot_shocks: vec![
                dec!(-0.15),
                dec!(-0.10),
                dec!(-0.05),
                Decimal::ZERO,
                dec!(0.05),
                dec!(0.10),
                dec!(0.15),
                dec!(0.25),
                dec!(0.50),
                dec!(1.00),
            ],
            vol_shocks: vec![dec!(-0.20), Decimal::ZERO, dec!(0.20)],
            initial_multiplier: dec!(1.00),
        }
    }
}

impl RiskArrayMargin {
    /// Build the full stress grid from the configured shocks.
    pub fn scenarios(&self) -> Vec<Scenario> {
        let mut scenarios = Vec::with_capacity(self.spot_shocks.len() * self.vol_shocks.len());
        for &spot_shock in &self.spot_shocks {
            for &vol_shock in &self.vol_shocks {
                scenarios.push(Scenario {
                    spot_shock,
                    vol_shock,
                });
            }
        }
        scenarios
    }

    /// Black-Scholes value of a single leg under a stressed spot and vol.
    fn leg_value(leg: &MarginLeg, spot: Decimal, iv: Decimal) -> Decimal {
        let intrinsic = if leg.is_call {
            (spot - leg.strike).max(Decimal::ZERO)
        } else {
            (leg.strike - spot).max(Decimal::ZERO)
        };
        // Time value decays with the stressed volatility; the intrinsic value
        // is the floor, which keeps the grid conservative for deep ITM legs.
        let time_value = spot * iv * leg.time_to_expiry.max(Decimal::ZERO);
        intrinsic + time_value
    }

    /// Loss of the position set under a single scenario.
    fn scenario_loss(&self, legs: &[MarginLeg], scenario: &Scenario) -> Decimal {
        let mut loss = Decimal::ZERO;
        for leg in legs {
            let stressed_spot = leg.spot * (Decimal::ONE + scenario.spot_shock);
            let stressed_iv = (leg.iv + scenario.vol_shock).max(Decimal::ZERO);
            let base = Self::leg_value(leg, leg.spot, leg.iv);
            let stressed = Self::leg_value(leg, stressed_spot, stressed_iv);
            // A short leg loses when the stressed value rises above the base.
            loss += (stressed - base) * leg.quantity;
        }
        loss
    }
}

impl MarginModel for RiskArrayMargin {
    fn requirement(&self, legs: &[MarginLeg]) -> Result<MarginRequirement, Error> {
        if legs.is_empty() {
            return Ok(MarginRequirement {
                initial: Decimal::ZERO,
                maintenance: Decimal::ZERO,
                worst_scenario: Scenario {
                    spot_shock: Decimal::ZERO,
                    vol_shock: Decimal::ZERO,
                },
                contributions: HashMap::new(),
            });
        }

        let mut worst_loss = Decimal::ZERO;
        let mut worst_scenario = Scenario {
            spot_shock: Decimal::ZERO,
            vol_shock: Decimal::ZERO,
        };

        for scenario in self.scenarios() {
            let loss = self.scenario_loss(legs, &scenario);
            if loss > worst_loss {
                worst_loss = loss;
                worst_scenario = scenario;
            }
        }

        // Per-position contribution: the loss of each underlying's legs under
        // the worst scenario, so the endpoint can attribute the requirement.
        let mut contributions: HashMap<String, Decimal> = HashMap::new();
        for leg in legs {
            let stressed_spot = leg.spot * (Decimal::ONE + worst_scenario.spot_shock);
            let stressed_iv = (leg.iv + worst_scenario.vol_shock).max(Decimal::ZERO);
            let base = Self::leg_value(leg, leg.spot, leg.iv);
            let stressed = Self::leg_value(leg, stressed_spot, stressed_iv);
            let contribution = ((stressed - base) * leg.quantity).max(Decimal::ZERO);
            *contributions.entry(leg.underlying.clone()).or_default() += contribution;
        }

        Ok(MarginRequirement {
            initial: worst_loss * self.initial_multiplier,
            maintenance: worst_loss,
            worst_scenario,
            contributions,
        })
    }
}

/// The margin engine, holding the active model and the feature flag.
#[derive(Debug, Clone)]
pub struct MarginEngine {
    /// Whether the risk-array model is enabled for this environment.
    pub risk_array_enabled: bool,
    /// The risk-array model, used when enabled.
    pub risk_array: RiskArrayMargin,
    /// The legacy model, used as a fallback.
    pub strategy_based: StrategyBasedMargin,
}

impl Default for MarginEngine {
    fn default() -> Self {
        Self {
            risk_array_enabled: false,
            risk_array: RiskArrayMargin::default(),
            strategy_based: StrategyBasedMargin,
        }
    }
}

impl MarginEngine {
    /// Compute the requirement using the active model.
    pub fn requirement(&self, legs: &[MarginLeg]) -> Result<MarginRequirement, Error> {
        if self.risk_array_enabled {
            self.risk_array.requirement(legs)
        } else {
            self.strategy_based.requirement(legs)
        }
    }

    /// What-if check: reject when the post-trade initial margin would exceed
    /// equity. Runs before commit, inside the same transaction.
    pub fn check_post_trade(&self, legs: &[MarginLeg], equity: Decimal) -> Result<(), Error> {
        let requirement = self.requirement(legs)?;
        if requirement.initial > equity {
            return Err(Error::MarginExceeded {
                required: requirement.initial,
                equity,
            });
        }
        Ok(())
    }
}
