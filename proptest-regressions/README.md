# proptest regressions

When a property test fails, proptest shrinks the failing input to a minimal
counterexample and writes it here as a `.txt` file named after the test
(e.g. `invariants_test.txt`). Every run replays the seeds in this directory
first, so a counterexample that was ever found is always re-checked.

These files are generated, not hand-written. Don't delete one to "make CI
green" — each is a real regression that `tests/invariants_test.rs` caught.
Once the underlying bug is fixed, the file stays and keeps guarding against
the regression.

Regression files are committed on purpose so the fix and its counterexample
land together.
