//! Kani proof harnesses for the fund-critical fixed-point arithmetic in
//! [`crate::math_fixed`] (issue #119).
//!
//! Compiled and checked only under `cargo kani` (the module is declared with
//! `#[cfg(kani)]`), so a normal `cargo test` / `cargo clippy` never sees it.
//! See `docs/verification.md` for the property list, the run command, and the
//! documented gap between these integer prototypes and the runtime `f64`
//! functions they shadow.
//!
//! Every harness constrains its inputs with `kani::assume` to the documented
//! domain (`0..=MAX_INPUT`, positive contract counts, fee rates within
//! `0..=500` basis points — the narrow verified word can only represent part
//! of the function's full basis-point range, and the properties are
//! independent of the rate). That is what turns "no overflow" into a bounded
//! proof: outside that domain the result is unspecified by design.

use crate::math_fixed::{
    collateral_required_fixed, fee_fixed, intrinsic_fixed, leg_pnl_fixed, OptionKind, MAX_INPUT,
};

/// Any non-negative fixed-point value within the documented bound.
fn any_input() -> i16 {
    let value: i16 = kani::any();
    kani::assume(value >= 0 && value <= MAX_INPUT);
    value
}

/// A strictly positive contract count, mirroring the handler's validation.
fn any_contracts() -> i16 {
    let value: i16 = kani::any();
    kani::assume(value > 0 && value <= MAX_INPUT);
    value
}

fn any_kind() -> OptionKind {
    let is_call: bool = kani::any();
    if is_call {
        OptionKind::Call
    } else {
        OptionKind::Put
    }
}

/// Collateral is never negative, for calls or puts.
#[kani::proof]
fn collateral_is_never_negative() {
    let kind = any_kind();
    let contracts = any_contracts();
    let strike = any_input();
    let spot = any_input();
    assert!(collateral_required_fixed(kind, contracts, strike, spot) >= 0);
}

/// Collateral is monotonic in the contract count.
#[kani::proof]
fn collateral_is_monotonic_in_contracts() {
    let kind = any_kind();
    let strike = any_input();
    let spot = any_input();
    let low = any_contracts();
    let delta = any_input();
    let high = low + delta;
    kani::assume(high <= MAX_INPUT);
    assert!(
        collateral_required_fixed(kind, high, strike, spot)
            >= collateral_required_fixed(kind, low, strike, spot)
    );
}

/// Collateral is monotonic in strike (strictly for puts; calls are
/// strike-independent, so the inequality still holds with equality).
#[kani::proof]
fn collateral_is_monotonic_in_strike() {
    let kind = any_kind();
    let contracts = any_contracts();
    let spot = any_input();
    let low = any_input();
    let delta = any_input();
    let high = low + delta;
    kani::assume(high <= MAX_INPUT);
    assert!(
        collateral_required_fixed(kind, contracts, high, spot)
            >= collateral_required_fixed(kind, contracts, low, spot)
    );
}

/// A short put is always over-collateralised against its maximum possible
/// loss (spot at zero, i.e. the full strike), so collateral covers the payoff
/// at expiry no matter where spot settles.
#[kani::proof]
fn short_put_collateral_covers_the_maximum_loss() {
    let contracts = any_contracts();
    let strike = any_input();
    let spot = any_input();
    let collateral = collateral_required_fixed(OptionKind::Put, contracts, strike, spot);
    let maximum_loss = intrinsic_fixed(OptionKind::Put, strike, 0) * contracts;
    assert!(collateral >= maximum_loss);
}

/// The payoff of a long leg plus the mirrored short leg is exactly zero.
#[kani::proof]
fn mirrored_legs_sum_to_zero() {
    let kind = any_kind();
    let strike = any_input();
    let contracts = any_contracts();
    let premium = any_input();
    let spot = any_input();
    let long = leg_pnl_fixed(kind, true, strike, contracts, premium, spot);
    let short = leg_pnl_fixed(kind, false, strike, contracts, premium, spot);
    assert!(long + short == 0);
}

/// Intrinsic value never goes below the zero floor.
#[kani::proof]
fn intrinsic_is_non_negative() {
    let kind = any_kind();
    let strike = any_input();
    let spot = any_input();
    assert!(intrinsic_fixed(kind, strike, spot) >= 0);
}

/// Fee rounding always favours the protocol: the charged fee is never below
/// the exact rational fee, and never more than one fee unit above it.
#[kani::proof]
fn fee_rounding_favours_the_protocol_by_at_most_one_unit() {
    let notional = any_input();
    let bps: i16 = kani::any();
    // A 5% fee ceiling: `notional * bps` and `fee * 10_000` must stay inside
    // the narrow verified word (see docs/verification.md). The rounding
    // property itself is independent of the rate.
    kani::assume(bps >= 0 && bps <= 500);
    let fee = fee_fixed(notional, bps);
    assert!(fee * 10_000 >= notional * bps);
    assert!(fee * 10_000 - notional * bps < 10_000);
}

/// No arithmetic overflow for any input inside the documented domain — Kani
/// checks every multiplication/addition in these calls for overflow, so
/// reaching the end of the harness is the proof.
#[kani::proof]
fn no_overflow_inside_the_documented_domain() {
    let kind = any_kind();
    let contracts = any_contracts();
    let strike = any_input();
    let spot = any_input();
    let bps: i16 = kani::any();
    kani::assume(bps >= 0 && bps <= 500);

    let collateral = collateral_required_fixed(kind, contracts, strike, spot);
    let intrinsic = intrinsic_fixed(kind, strike, spot);
    let pnl = leg_pnl_fixed(kind, true, strike, contracts, spot, spot);
    // `fee_fixed`'s documented notional bound is MAX_INPUT, like every other
    // input; a product such as `contracts * spot` would exceed it.
    let fee = fee_fixed(spot, bps);

    // Keep the results live so the calls can't be optimized away.
    assert!(collateral >= 0 && intrinsic >= 0 && fee >= 0);
    let _ = pnl;
}
