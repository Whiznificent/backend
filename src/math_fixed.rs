//! Fixed-point (integer-scaled) prototypes of the fund-critical arithmetic,
//! used as the bounded-domain target of the Kani proofs in
//! [`crate::kani_proofs`] (issue #119).
//!
//! The runtime still prices and settles in `f64` (`BSInputs`/`BSResult`), and
//! bounded model checking over `f64` Black-Scholes is not tractable. These
//! functions are therefore exact, overflow-checkable integer analogues of
//! `collateral::collateral_required`, the payoff math in `payoff.rs`, and the
//! protocol-fee arithmetic, expressed in units of [`SCALE`] (1/100).
//! `docs/verification.md` records exactly which runtime functions they
//! shadow and what the fixed-point migration would need to prove directly.
//!
//! Every rounding step is deliberately **up** ("protocol-favouring"): a
//! covered call locks the full spot value, a cash-secured put locks at least
//! 110% of strike, and fees round up, so rounding can never leave the
//! protocol short.

/// Number of fixed-point units per whole token. The prototypes deliberately
/// use a narrow word (`i16`) with a small documented domain: Kani's SAT
/// encoding of symbolic multiply/divide blows up superlinearly with bit
/// width, and none of the proven properties depend on magnitude — shrinking
/// the word keeps every proof fully symbolic while finishing in minutes
/// rather than hours. See `docs/verification.md`.
pub const SCALE: i16 = 100;

/// Largest value any input may take, in fixed-point units. Chosen so the
/// widest intermediate product (`MAX_INPUT * MAX_INPUT * 11 = 27_500`) stays
/// inside `i16` (`i16::MAX = 32_767`). The Kani proofs
/// constrain their inputs to `0..=MAX_INPUT`, which is what makes "no
/// arithmetic overflow inside the documented domain" a theorem rather than
/// a hope.
pub const MAX_INPUT: i16 = 50;

/// Decomposed (scaled) values are rounded away from zero, never toward it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    Call,
    Put,
}

/// `value * numerator / denominator`, rounded **up** to the next unit —
/// never below the exact rational quotient, so rounding favours the
/// protocol. Signed `i16::div_ceil` is still an unstable library feature
/// (`int_roundings`), hence the explicit quotient/remainder round-up.
fn mul_div_ceil(value: i16, numerator: i16, denominator: i16) -> i16 {
    let product = value * numerator;
    let quotient = product / denominator;
    if product % denominator == 0 {
        quotient
    } else {
        quotient + 1
    }
}

/// Fixed-point analogue of `crate::collateral::collateral_required`:
/// covered calls lock 100% of the current spot value, cash-secured puts lock
/// 110% of strike (rounded up). Only the write/short side requires collateral.
pub fn collateral_required_fixed(kind: OptionKind, contracts: i16, strike: i16, spot: i16) -> i16 {
    match kind {
        OptionKind::Call => contracts * spot,
        OptionKind::Put => mul_div_ceil(contracts * strike, 11, 10),
    }
}

/// Intrinsic value of one contract at expiry, in fixed-point units. Never
/// negative — the payoff floor is exactly zero.
pub fn intrinsic_fixed(kind: OptionKind, strike: i16, spot: i16) -> i16 {
    match kind {
        OptionKind::Call => (spot - strike).max(0),
        OptionKind::Put => (strike - spot).max(0),
    }
}

/// Per-leg P&L at expiry, mirroring `payoff::combined_pnl`: a long leg is
/// worth `intrinsic - premium`, a short leg the exact negative of that.
pub fn leg_pnl_fixed(
    kind: OptionKind,
    is_long: bool,
    strike: i16,
    contracts: i16,
    premium: i16,
    spot_at_expiry: i16,
) -> i16 {
    let intrinsic = intrinsic_fixed(kind, strike, spot_at_expiry);
    let per_contract = if is_long {
        intrinsic - premium
    } else {
        premium - intrinsic
    };
    per_contract * contracts
}

/// Protocol fee of `bps` basis points on `notional`, rounded **up** so
/// rounding always favours the protocol.
pub fn fee_fixed(notional: i16, bps: i16) -> i16 {
    mul_div_ceil(notional, bps, 10_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_collateral_is_one_hundred_percent_of_spot() {
        assert_eq!(
            collateral_required_fixed(OptionKind::Call, 2, 70, 67),
            2 * 67
        );
    }

    #[test]
    fn put_collateral_is_one_hundred_ten_percent_of_strike() {
        // 3 * 60 * 1.1 == 198, exactly.
        assert_eq!(collateral_required_fixed(OptionKind::Put, 3, 60, 50), 198);
    }

    #[test]
    fn put_collateral_rounds_up_never_down() {
        // 1 * 1 * 1.1 == 1.1 -> rounds up to 2, never truncates to 1.
        assert_eq!(collateral_required_fixed(OptionKind::Put, 1, 1, 1), 2);
    }

    #[test]
    fn intrinsic_is_never_negative() {
        assert_eq!(intrinsic_fixed(OptionKind::Call, 100, 120), 20);
        assert_eq!(intrinsic_fixed(OptionKind::Call, 120, 100), 0);
        assert_eq!(intrinsic_fixed(OptionKind::Put, 100, 80), 20);
        assert_eq!(intrinsic_fixed(OptionKind::Put, 80, 100), 0);
    }

    #[test]
    fn mirrored_legs_sum_to_zero() {
        let long = leg_pnl_fixed(OptionKind::Call, true, 100, 2, 5, 130);
        let short = leg_pnl_fixed(OptionKind::Call, false, 100, 2, 5, 130);
        assert_eq!(long + short, 0);
    }

    #[test]
    fn short_put_collateral_covers_even_the_maximum_loss() {
        let collateral = collateral_required_fixed(OptionKind::Put, 2, 100, 500);
        // Worst case for a short put is spot at zero, i.e. the full strike.
        let maximum_loss = intrinsic_fixed(OptionKind::Put, 100, 0) * 2;
        assert!(collateral >= maximum_loss);
    }

    #[test]
    fn fee_rounds_up_favouring_the_protocol() {
        // 100 * 250bps == 2.5 -> rounds up to 3, never truncates to 2.
        assert_eq!(fee_fixed(100, 250), 3);
        // 1 unit at 1bp is 0.01 -> rounds up to a whole unit.
        assert_eq!(fee_fixed(1, 1), 1);
        assert_eq!(fee_fixed(0, 10_000), 0);
    }
}
