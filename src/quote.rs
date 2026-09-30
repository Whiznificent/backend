//! Two-sided option quote model.
//!
//! Replaces single-price execution with a bid/ask/mid/mark quote. The mid is
//! the theoretical Black-Scholes premium; the spread is expressed in vol
//! points and scaled by time-to-expiry and moneyness (wings), with a minimum
//! absolute tick so near-zero-premium deep-OTM options still carry a spread.
//!
//! Users buy at the ask and sell at the bid; mark-to-market uses the mid.

use crate::{BSInputs, BSResult};
use serde::{Deserialize, Serialize};

/// Configurable spread model.
///
/// * `base_spread_vol` — base spread in vol points (e.g. 0.02 = 2 vol points).
/// * `min_tick` — minimum absolute spread (in premium units) applied to each
///   side, so deep-OTM options with near-zero premium still have a spread.
/// * `time_multiplier` — scales the spread as time-to-expiry shrinks; short
///   dated options are riskier to quote, so the spread widens.
/// * `wing_multiplier` — scales the spread for far-from-the-money strikes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteModel {
    pub base_spread_vol: f64,
    pub min_tick: f64,
    pub time_multiplier: f64,
    pub wing_multiplier: f64,
}

impl Default for QuoteModel {
    fn default() -> Self {
        Self {
            base_spread_vol: 0.02, // 2 vol points
            min_tick: 0.0005,
            time_multiplier: 0.5,
            wing_multiplier: 1.0,
        }
    }
}

/// A two-sided quote for a single option.
#[derive(Debug, Clone, Serialize)]
pub struct Quote {
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    pub mark: f64,
}

/// Compute a two-sided quote from a Black-Scholes result.
///
/// The mid is the theoretical premium. The half-spread is derived from the
/// model's base spread (converted to premium via vega), then scaled by
/// time-to-expiry and moneyness. A minimum absolute tick is enforced on each
/// side. The bid is floored at intrinsic minus a tolerance and never below
/// zero; at expiry bid == ask == intrinsic.
pub fn quote(bs: &BSResult, inputs: &BSInputs, model: &QuoteModel) -> Quote {
    let mid = bs.premium;

    // At expiry (or no time value) the option is worth exactly its intrinsic
    // value: bid == ask == intrinsic.
    if inputs.t <= 0.0 {
        let intrinsic = bs.intrinsic.max(0.0);
        return Quote {
            bid: intrinsic,
            ask: intrinsic,
            mid: intrinsic,
            mark: intrinsic,
        };
    }

    // Base spread in premium units: vol points * vega (vega is per 1% vol, so
    // multiply by 100 to convert vol points to a 1.0-vol fraction).
    let base_premium = model.base_spread_vol * bs.vega * 100.0;

    // Time-to-expiry multiplier: widen as expiry approaches. `t` is in years;
    // scale relative to a ~30 day reference so a month out is ~1x.
    let ref_t = 30.0 / 365.0;
    let time_scale = 1.0 + model.time_multiplier * (ref_t / inputs.t.max(1e-6) - 1.0).max(0.0);

    // Wing multiplier: widen for far-from-the-money strikes.
    let moneyness = if inputs.strike > 0.0 {
        inputs.spot / inputs.strike
    } else {
        1.0
    };
    let wing = (moneyness - 1.0).abs();
    let wing_scale = 1.0 + model.wing_multiplier * wing;

    let half_spread = (base_premium * time_scale * wing_scale).max(model.min_tick);

    let mut bid = mid - half_spread;
    let ask = mid + half_spread;

    // Bid never below intrinsic minus a tolerance, and never below zero.
    let tolerance = model.min_tick;
    let floor = (bs.intrinsic - tolerance).max(0.0);
    if bid < floor {
        bid = floor;
    }

    Quote {
        bid,
        ask,
        mid,
        mark: mid,
    }
}
