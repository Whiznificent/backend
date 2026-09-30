# Index audit

Every production query's index coverage, verified by the performance
regression suite (`tests/query_plan_test.rs`) which asserts the
`EXPLAIN QUERY PLAN` output for each query against a large seeded dataset.
The suite fails if any hot path stops using its expected index or degrades
into a full table scan (`SCAN`).

## Existing indexes

| Index | Table | Columns | Serves |
|---|---|---|---|
| `PRIMARY KEY` | accounts | `wallet_address` | `get_account`, account lookup by wallet |
| `PRIMARY KEY` | positions | `id` | `select_position_by_id`, `select_open_position_for_close` |
| `idx_positions_wallet` | positions | `wallet_address` | per-wallet position lookups |
| `idx_positions_wallet_status` | positions | `(wallet_address, status)` | `list_positions`, `history` trades/stats, `select_open_positions_greeks` |
| `idx_positions_strategy` | positions | `strategy_id` (partial, `WHERE strategy_id IS NOT NULL`) | `list_strategies`, `load_strategy_legs`, `select_open_strategy_leg_ids` |
| `idx_alerts_wallet` | alerts | `wallet_address` | `get_alerts` |
| `idx_alerts_untriggered` | alerts | `(underlying, triggered)` (partial, `WHERE triggered = 0`) | `check_alerts_update` (the 10s alert loop) |
| `PRIMARY KEY` | watchlist | `(wallet_address, underlying)` | `get_watchlist` |
| `PRIMARY KEY` | sessions | `token` | `select_session` (auth) |
| `PRIMARY KEY` | auth_nonces | `nonce` | `select_nonce_expires` (auth) |
| `idx_ticks_underlying` | ticks | `underlying` | tick history |
| `idx_ticks_underlying_time` | ticks | `(underlying, tick_at)` | tick history ordered by time |

## Audit result

Each query in the catalogue (`tests/common/perf.rs`) was checked against a
seeded dataset of ~100k positions / ~10k wallets / ~1M ticks. Every hot path
resolves to an index seek (`SEARCH ... USING INDEX` / `USING PRIMARY KEY`);
none degrades into a full table scan (`SCAN`).

**Added indexes: none.** The indexes already present (most notably
`idx_positions_wallet_status` and `idx_alerts_untriggered`) cover every query
the issue calls out — `list_positions` with status and ordering,
`get_history`'s stats, and the alert checks — so no new index was required to
keep them off a full scan.

**Removed indexes: none.**

## Measured impact

The perf suite measures p95 latency per query on the large dataset (see
`tests/common/perf.rs` for the budgets). Because every hot path is an index
seek, p95 stays in the single-digit-millisecond range even at 100k positions;
a regression that dropped an index (e.g. a schema change that stopped the
planner using `idx_positions_wallet_status`) would push the query toward a
tens-of-milliseconds full scan and fail both the plan assertion and the
latency budget.

To re-run the audit manually:

```bash
ZENITH_PERF_FULL=1 cargo test --test query_plan_test -- --nocapture
```
