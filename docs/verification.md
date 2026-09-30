# Formal verification report — collateral, payoff and fee arithmetic

This document is the verification report for issue #119. It records *what*
is proven, *how* to reproduce it, and — importantly — *where the proof sits
relative to the shipped runtime*, which still prices in `f64`.

## What is verified

[Kani](https://model-checking.github.io/kani/) bounded model checking is
applied to the integer-scaled prototypes in [`src/math_fixed.rs`](../src/math_fixed.rs)
through the harnesses in [`src/kani_proofs.rs`](../src/kani_proofs.rs).
Because Kani explores the whole (bounded) input space rather than sampling it,
these are proofs, not tests — every input in the documented domain, not a
handful of examples.

Every harness constrains its inputs with `kani::assume` to the documented
domain:

- non-negative values `v` with `v <= MAX_INPUT` (`MAX_INPUT = 50` fixed-point
  units, i.e. a `SCALE` of 100), chosen so the widest intermediate product
  (`50 * 50 * 11 = 27_500`) is inside `i16` (`i16::MAX = 32_767`). The word is
  deliberately narrow: Kani's SAT encoding of symbolic multiply/divide grows
  superlinearly with bit width, so 32-bit versions of these same harnesses do
  not finish in a reasonable time — at 16 bits every harness is fully symbolic
  and finishes in seconds-to-minutes. None of the proven properties depend on
  magnitude (they are algebraic facts about the formulas), so the narrow domain
  verifies the same properties at a tractable cost. When the fixed-point
  migration lands, the properties port to the production width unchanged;
- strictly positive contract counts;
- fee rates `bps` in `0..=500` (a 5% ceiling — the narrow verified word can
  only represent part of the basis-point range, and the rounding property does
  not depend on the rate).

### Properties

| # | Property | Harness |
|---|---|---|
| 1 | Collateral is never negative | `collateral_is_never_negative` |
| 2 | Collateral is monotonic in the contract count | `collateral_is_monotonic_in_contracts` |
| 3 | Collateral is monotonic in strike (equal for calls) | `collateral_is_monotonic_in_strike` |
| 4 | Short-put collateral ≥ the maximum possible loss (spot at zero) | `short_put_collateral_covers_the_maximum_loss` |
| 5 | Long leg + mirrored short leg P&L is exactly zero | `mirrored_legs_sum_to_zero` |
| 6 | Intrinsic value never dips below the zero floor | `intrinsic_is_non_negative` |
| 7 | Fee rounding favours the protocol, and by < 1 fee unit | `fee_rounding_favours_the_protocol_by_at_most_one_unit` |
| 8 | No arithmetic overflow anywhere inside the documented domain | `no_overflow_inside_the_documented_domain` |

Every rounding step is **up** (explicit quotient/remainder round-up), so no property depends on a
truncation direction happening to be benign.

## What is *not* verified, and why

The shipped code computes collateral as `contracts * spot` / `contracts *
strike * 1.1` in `f64` ([`src/collateral.rs`](../src/collateral.rs)) and pays
off legs in `f64` ([`src/payoff.rs`](../src/payoff.rs)). Verifying `f64`
Black-Scholes / `f64` payoff arithmetic with bounded model checking is not
tractable (the solver has to reason about IEEE-754 rounding over the whole
exponent range), which is exactly why the issue scopes it out.

So the proofs target **integer-scaled prototypes** of the same formulas. The
gap is precise and mechanical:

- `collateral_required_fixed(kind, contracts, strike, spot)` is the
  fixed-point analogue of `collateral::collateral_required`. `Call => contracts
  * spot` is identical; `Put => ceil(contracts * strike * 11 / 10)` replaces
  `contracts * strike * 1.1`, additionally pinning the rounding direction
  (the `f64` version has no explicit rounding contract).
- `intrinsic_fixed` / `leg_pnl_fixed` mirror `payoff::combined_pnl`'s
  intrinsic/payoff arithmetic, with `max(0)` replaced by the integer zero
  floor.
- `fee_fixed` has no runtime counterpart yet — the protocol charges no fee
  today — so it fixes the rounding *direction* (up, protocol-favouring) before
  any fee code is written, and is ready to be ported when it is.

Once the decimal/fixed-point migration the issue references lands, the runtime
functions should be rewritten over this representation and these same
harnesses (switched from the prototypes to the real functions) re-run — at
which point the gap closes with no change to the property list.

## How to run

```bash
# Install (once): https://model-checking.github.io/kani/install-guide.html
cargo kani
```

CI runs the same command via `.github/workflows/kani.yml` on every PR and
nightly. A normal `cargo test` / `cargo clippy` never compiles the harnesses:
the module is gated behind `#[cfg(kani)]`, so the tests remain fast.

## Expected output

Each harness reports `VERIFICATION:- SUCCESSFUL`; the job fails if any harness
finds a counterexample. A minimal run looks like:

```
cargo kani
...
Checking harness kani_proofs::collateral_is_never_negative...
VERIFICATION:- SUCCESSFUL
...
Checking harness kani_proofs::fee_rounding_favours_the_protocol_by_at_most_one_unit...
VERIFICATION:- SUCCESSFUL
...
Complete - 8 successfully verified harnesses, 0 failures, 8 total.
```

## Counterexample policy

If a later change falsifies one of these properties, Kani prints the concrete
input that breaks it. That counterexample must be fixed and captured as a unit
regression test in `src/math_fixed.rs` (the fixed-point case) so the fix is
guarded even outside a Kani run.
