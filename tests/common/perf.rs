//! Deterministic large-dataset seed generator, the catalogue of named
//! production queries, and the plan/latency assertion helpers for the
//! performance regression suite.
//!
//! The full suite (see `tests/query_plan_test.rs`) seeds ~100k positions,
//! ~10k wallets and ~1M ticks, runs every production query, and asserts on
//! (1) the EXPLAIN QUERY PLAN output — that each hot path uses its expected
//! index and never degrades into a full table scan — and (2) a p95 latency
//! budget. It is gated behind `ZENITH_PERF_FULL` so it only runs in the
//! nightly CI job; a fast smoke subset runs on every PR.

#![allow(dead_code)]

use sqlx::SqlitePool;

/// A representative bind value for a query, used both to render a SQL
/// literal for `EXPLAIN QUERY PLAN` and to bind the real query for latency.
#[derive(Clone, Copy)]
pub enum BindValue {
    Int(i64),
    Real(f64),
    Text(&'static str),
    Null,
}

impl BindValue {
    /// Renders the value as a SQL literal (for EXPLAIN QUERY PLAN).
    fn literal(self) -> String {
        match self {
            BindValue::Int(i) => i.to_string(),
            BindValue::Real(r) => r.to_string(),
            BindValue::Text(s) => format!("'{}'", s.replace('\'', "''")),
            BindValue::Null => "NULL".to_string(),
        }
    }
}

/// Substitutes every `?` in `sql` with the corresponding literal.
fn substitute_literals(sql: &str, binds: &[BindValue]) -> String {
    let mut out = String::with_capacity(sql.len() + binds.len() * 8);
    let mut binds = binds.iter();
    for c in sql.chars() {
        if c == '?' {
            match binds.next() {
                Some(b) => out.push_str(&b.literal()),
                None => out.push('?'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Binds the values to a runtime query.
fn bind_all<'a>(
    mut query: sqlx::query::Query<'a, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'a>>,
    binds: &'a [BindValue],
) -> sqlx::query::Query<'a, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'a>> {
    for b in binds {
        query = match b {
            BindValue::Int(i) => query.bind(*i),
            BindValue::Real(r) => query.bind(*r),
            BindValue::Text(s) => query.bind(*s),
            BindValue::Null => query.bind(None::<String>),
        };
    }
    query
}

/// A named production query with its expected plan and latency budget.
pub struct QuerySpec {
    pub name: &'static str,
    /// The production SQL, with `?` binds.
    pub sql: &'static str,
    /// Representative bind values.
    pub binds: &'static [BindValue],
    /// A specific index (or "PRIMARY KEY") the plan must use. `None` only
    /// requires that the plan is an index seek with no full table scan — used
    /// where the planner has more than one index it could legitimately pick.
    pub expected_index: Option<&'static str>,
    /// p95 latency budget in microseconds (full suite only).
    pub p95_budget_us: u64,
}

/// Deterministically seeds `wallets` accounts, `positions` positions and
/// `ticks` tick rows. The same arguments always produce the same data (the
/// recursive CTEs are fully determined by the counts), so plan and latency
/// measurements are comparable across runs.
pub async fn seed(pool: &SqlitePool, wallets: usize, positions: usize, ticks: usize) {
    sqlx::query(
        "INSERT INTO accounts (wallet_address)
         WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM cnt WHERE x < ?)
         SELECT 'WALLET' || printf('%08d', x) FROM cnt",
    )
    .bind(wallets as i64)
    .execute(pool)
    .await
    .unwrap();

    // Positions are spread across wallets with varied underlyings, strikes,
    // types and statuses. Closed/rolled rows carry settlement columns so the
    // status/column coherence CHECK (migration 0007) is satisfied.
    sqlx::query(
        "INSERT INTO positions (id, wallet_address, underlying, strike, expiry_days, option_type, position_type, contracts, entry_premium, entry_spot, collateral, status, close_premium, close_spot, realized_pnl, opened_at, closed_at)
         WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM cnt WHERE x < ?)
         SELECT
             'P' || printf('%08d', x),
             'WALLET' || printf('%08d', (x % ?) + 1),
             CASE (x % 4) WHEN 0 THEN 'BTC' WHEN 1 THEN 'ETH' WHEN 2 THEN 'SOL' ELSE 'XLM' END,
             50000 + (x % 50) * 1000,
             30,
             CASE (x % 2) WHEN 0 THEN 'call' ELSE 'put' END,
             CASE (x % 2) WHEN 0 THEN 'long' ELSE 'short' END,
             1 + (x % 10),
             100 + (x % 100),
             67420.5,
             0,
             CASE (x % 3) WHEN 0 THEN 'open' WHEN 1 THEN 'closed' ELSE 'rolled' END,
             CASE (x % 3) WHEN 0 THEN NULL ELSE 50 END,
             CASE (x % 3) WHEN 0 THEN NULL ELSE 67420.5 END,
             CASE (x % 3) WHEN 0 THEN NULL ELSE (x % 20) - 10 END,
             '2024-01-01T00:00:00.000Z',
             CASE (x % 3) WHEN 0 THEN NULL ELSE '2024-06-01T00:00:00.000Z' END
         FROM cnt",
    )
    .bind(positions as i64)
    .bind(wallets as i64)
    .execute(pool)
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO ticks (underlying, spot, vol)
         WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM cnt WHERE x < ?)
         SELECT 'BTC', 67420.5, 0.65 FROM cnt",
    )
    .bind(ticks as i64)
    .execute(pool)
    .await
    .unwrap();
}

/// Returns the EXPLAIN QUERY PLAN output for a query with its binds rendered
/// as literals (the plan is determined by the query shape and schema, so
/// literals are representative).
pub async fn explain_plan(pool: &SqlitePool, spec: &QuerySpec) -> String {
    let sql = substitute_literals(spec.sql, spec.binds);
    let rows = sqlx::query(&format!("EXPLAIN QUERY PLAN {sql}"))
        .fetch_all(pool)
        .await
        .unwrap();
    let mut plan = String::new();
    for row in &rows {
        let detail: String = row.get(3);
        if !plan.is_empty() {
            plan.push('\n');
        }
        plan.push_str(&detail);
    }
    plan
}

/// Asserts the plan is an index seek (never a full table scan) and, when a
/// specific index is expected, that it uses it.
pub fn assert_plan(spec: &QuerySpec, plan: &str) {
    assert!(
        plan.contains("USING"),
        "{}: expected an index seek, got:\n{}",
        spec.name,
        plan
    );
    assert!(
        !plan.contains("SCAN"),
        "{}: plan must not contain a full table scan, got:\n{}",
        spec.name,
        plan
    );
    if let Some(expected) = spec.expected_index {
        assert!(
            plan.contains(expected),
            "{}: expected plan to use '{expected}', got:\n{}",
            spec.name,
            plan
        );
    }
}

/// Runs a query `iters` times and returns the p95 latency in microseconds.
pub async fn p95_latency_us(pool: &SqlitePool, spec: &QuerySpec, iters: u32) -> u64 {
    let mut times = Vec::with_capacity(iters as usize);
    for _ in 0..iters {
        let start = std::time::Instant::now();
        bind_all(sqlx::query(spec.sql), spec.binds)
            .fetch_all(pool)
            .await
            .unwrap();
        times.push(start.elapsed().as_micros() as u64);
    }
    times_unchecked_p95(&mut times)
}

fn times_unchecked_p95(times: &mut [u64]) -> u64 {
    times.sort_unstable();
    let idx = ((times.len() as f64) * 0.95).ceil() as usize;
    times[idx.saturating_sub(1)]
}

/// The catalogue of named production queries. Each entry mirrors a query the
/// application actually runs (the SQL matches the checked macros in `src/`),
/// with the index the planner should use and a p95 latency budget generous
/// enough to avoid CI flakiness while still catching a regression into a full
/// scan (which on 100k rows is tens of milliseconds).
pub static QUERY_CATALOGUE: &[QuerySpec] = &[
    QuerySpec {
        name: "list_positions_by_wallet_and_status",
        sql: "SELECT id, wallet_address, underlying, strike, expiry_days, option_type, position_type, contracts, entry_premium, entry_spot, collateral, status, close_premium, close_spot, realized_pnl, opened_at, closed_at, strategy_id FROM positions WHERE wallet_address = ? AND (? IS NULL OR status = ?) AND (? IS NULL OR strategy_id = ?) ORDER BY opened_at DESC LIMIT ? OFFSET ?",
        binds: &[
            BindValue::Text("WALLET00000001"),
            BindValue::Text("open"),
            BindValue::Text("open"),
            BindValue::Null,
            BindValue::Null,
            BindValue::Int(50),
            BindValue::Int(0),
        ],
        expected_index: Some("idx_positions_wallet_status"),
        p95_budget_us: 5_000,
    },
    QuerySpec {
        name: "list_positions_by_wallet",
        sql: "SELECT id, wallet_address, underlying, strike, expiry_days, option_type, position_type, contracts, entry_premium, entry_spot, collateral, status, close_premium, close_spot, realized_pnl, opened_at, closed_at, strategy_id FROM positions WHERE wallet_address = ? AND (? IS NULL OR status = ?) AND (? IS NULL OR strategy_id = ?) ORDER BY opened_at DESC LIMIT ? OFFSET ?",
        binds: &[
            BindValue::Text("WALLET00000001"),
            BindValue::Null,
            BindValue::Null,
            BindValue::Null,
            BindValue::Null,
            BindValue::Int(50),
            BindValue::Int(0),
        ],
        // No status filter: the planner may pick either wallet_address index.
        expected_index: Some("idx_positions_wallet"),
        p95_budget_us: 5_000,
    },
    QuerySpec {
        name: "history_trades",
        sql: "SELECT id, wallet_address, underlying, strike, expiry_days, option_type, position_type, contracts, entry_premium, entry_spot, collateral, status, close_premium, close_spot, realized_pnl, opened_at, closed_at, strategy_id FROM positions WHERE wallet_address = ? AND status IN ('closed', 'rolled') ORDER BY closed_at DESC LIMIT ? OFFSET ?",
        binds: &[
            BindValue::Text("WALLET00000001"),
            BindValue::Int(50),
            BindValue::Int(0),
        ],
        expected_index: Some("idx_positions_wallet_status"),
        p95_budget_us: 5_000,
    },
    QuerySpec {
        name: "history_stats",
        sql: "SELECT COUNT(*), COALESCE(SUM(CASE WHEN realized_pnl > 0 THEN 1 ELSE 0 END), 0), COALESCE(SUM(CASE WHEN realized_pnl < 0 THEN 1 ELSE 0 END), 0), SUM(realized_pnl) FROM positions WHERE wallet_address = ? AND status IN ('closed', 'rolled')",
        binds: &[BindValue::Text("WALLET00000001")],
        expected_index: Some("idx_positions_wallet_status"),
        p95_budget_us: 10_000,
    },
    QuerySpec {
        name: "select_open_positions_greeks",
        sql: "SELECT id, wallet_address, underlying, strike, expiry_days, option_type, position_type, contracts, entry_premium, entry_spot, collateral, status, close_premium, close_spot, realized_pnl, opened_at, closed_at, strategy_id FROM positions WHERE wallet_address = ? AND status = 'open'",
        binds: &[BindValue::Text("WALLET00000001")],
        expected_index: Some("idx_positions_wallet_status"),
        p95_budget_us: 5_000,
    },
    QuerySpec {
        name: "list_strategies",
        sql: "SELECT id, wallet_address, underlying, strike, expiry_days, option_type, position_type, contracts, entry_premium, entry_spot, collateral, status, close_premium, close_spot, realized_pnl, opened_at, closed_at, strategy_id FROM positions WHERE wallet_address = ? AND strategy_id IS NOT NULL ORDER BY opened_at ASC",
        binds: &[BindValue::Text("WALLET00000001")],
        // strategy_id IS NOT NULL is not a seek, so the planner may use either
        // the wallet_status or the partial strategy index.
        expected_index: None,
        p95_budget_us: 10_000,
    },
    QuerySpec {
        name: "load_strategy_legs",
        sql: "SELECT id, wallet_address, underlying, strike, expiry_days, option_type, position_type, contracts, entry_premium, entry_spot, collateral, status, close_premium, close_spot, realized_pnl, opened_at, closed_at, strategy_id FROM positions WHERE wallet_address = ? AND strategy_id = ? ORDER BY opened_at ASC",
        binds: &[BindValue::Text("WALLET00000001"), BindValue::Text("STRAT0001")],
        expected_index: Some("idx_positions_strategy"),
        p95_budget_us: 5_000,
    },
    QuerySpec {
        name: "select_open_strategy_leg_ids",
        sql: "SELECT id FROM positions WHERE wallet_address = ? AND strategy_id = ? AND status = 'open'",
        binds: &[BindValue::Text("WALLET00000001"), BindValue::Text("STRAT0001")],
        expected_index: Some("idx_positions_strategy"),
        p95_budget_us: 5_000,
    },
    QuerySpec {
        name: "get_account",
        sql: "SELECT wallet_address, balance, collateral_locked, created_at FROM accounts WHERE wallet_address = ?",
        binds: &[BindValue::Text("WALLET00000001")],
        expected_index: Some("PRIMARY KEY"),
        p95_budget_us: 1_000,
    },
    QuerySpec {
        name: "select_position_by_id",
        sql: "SELECT id, wallet_address, underlying, strike, expiry_days, option_type, position_type, contracts, entry_premium, entry_spot, collateral, status, close_premium, close_spot, realized_pnl, opened_at, closed_at, strategy_id FROM positions WHERE id = ?",
        binds: &[BindValue::Text("P00000001")],
        expected_index: Some("PRIMARY KEY"),
        p95_budget_us: 1_000,
    },
    QuerySpec {
        name: "select_open_position_for_close",
        sql: "SELECT id, wallet_address, underlying, strike, expiry_days, option_type, position_type, contracts, entry_premium, entry_spot, collateral, status, close_premium, close_spot, realized_pnl, opened_at, closed_at, strategy_id FROM positions WHERE id = ? AND wallet_address = ? AND status = 'open'",
        binds: &[BindValue::Text("P00000001"), BindValue::Text("WALLET00000001")],
        expected_index: Some("PRIMARY KEY"),
        p95_budget_us: 1_000,
    },
    QuerySpec {
        name: "get_alerts",
        sql: "SELECT id, wallet_address, underlying, condition, target_price, triggered, created_at, triggered_at FROM alerts WHERE wallet_address = ? ORDER BY created_at DESC",
        binds: &[BindValue::Text("WALLET00000001")],
        expected_index: Some("idx_alerts_wallet"),
        p95_budget_us: 1_000,
    },
    QuerySpec {
        name: "check_alerts_update",
        sql: "UPDATE alerts SET triggered = 1, triggered_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE underlying = ? AND triggered = 0 AND ((condition = 'above' AND target_price <= ?) OR (condition = 'below' AND target_price >= ?))",
        binds: &[BindValue::Text("BTC"), BindValue::Real(65000.0), BindValue::Real(65000.0)],
        expected_index: Some("idx_alerts_untriggered"),
        p95_budget_us: 1_000,
    },
    QuerySpec {
        name: "get_watchlist",
        sql: "SELECT wallet_address, underlying, added_at FROM watchlist WHERE wallet_address = ? ORDER BY added_at DESC",
        binds: &[BindValue::Text("WALLET00000001")],
        expected_index: Some("PRIMARY KEY"),
        p95_budget_us: 1_000,
    },
    QuerySpec {
        name: "select_session",
        sql: "SELECT wallet_address, expires_at FROM sessions WHERE token = ?",
        binds: &[BindValue::Text("sometoken")],
        expected_index: Some("PRIMARY KEY"),
        p95_budget_us: 1_000,
    },
    QuerySpec {
        name: "select_nonce_expires",
        sql: "SELECT expires_at FROM auth_nonces WHERE nonce = ? AND wallet_address = ?",
        binds: &[BindValue::Text("somenonce"), BindValue::Text("WALLET00000001")],
        expected_index: Some("PRIMARY KEY"),
        p95_budget_us: 1_000,
    },
];
