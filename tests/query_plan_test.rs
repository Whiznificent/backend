//! Performance regression suite.
//!
//! `smoke_query_plans_use_indexes` runs on every PR: it seeds a small
//! dataset and asserts every production query's EXPLAIN QUERY PLAN uses its
//! expected index (no full table scans on the hot paths).
//!
//! `full_suite_query_plans_and_latency` is the nightly job: it seeds ~100k
//! positions / ~10k wallets / ~1M ticks and additionally asserts a p95
//! latency budget per query. It is gated behind `ZENITH_PERF_FULL` so it only
//! runs where the nightly workflow sets that variable.

mod common;

use common::perf;
use zenith_backend::db::init_pool;

/// The full suite is too slow for every PR; the nightly CI job opts in by
/// setting ZENITH_PERF_FULL.
fn full_suite_enabled() -> bool {
    std::env::var("ZENITH_PERF_FULL").is_ok()
}

async fn seeded_db(
    wallets: usize,
    positions: usize,
    ticks: usize,
) -> (sqlx::SqlitePool, std::path::PathBuf) {
    let db_path = std::env::temp_dir().join(format!(
        "zenith-query-plan-test-{}.db",
        uuid::Uuid::new_v4()
    ));
    let pool = init_pool(&format!("sqlite://{}", db_path.display())).await;
    perf::seed(&pool, wallets, positions, ticks).await;
    seed_auxiliary(&pool).await;
    (pool, db_path)
}

/// Inserts rows into the auxiliary tables so their plans are computed against
/// real data (a near-empty table can make the planner choose a trivial scan
/// that says nothing about index usage). Each table gets ~100 rows via a
/// recursive CTE; the first row uses the well-known fixture values the
/// catalogue queries look up.
async fn seed_auxiliary(pool: &sqlx::SqlitePool) {
    sqlx::query(
        "INSERT INTO alerts (id, wallet_address, underlying, condition, target_price)
         WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM cnt WHERE x < 100)
         SELECT 'a' || printf('%03d', x),
                'WALLET' || printf('%08d', (x % 100) + 1),
                CASE (x % 4) WHEN 0 THEN 'BTC' WHEN 1 THEN 'ETH' WHEN 2 THEN 'SOL' ELSE 'XLM' END,
                CASE (x % 2) WHEN 0 THEN 'above' ELSE 'below' END,
                50000 + (x % 30) * 1000
         FROM cnt",
    )
    .execute(pool).await.unwrap();
    // The catalogue's get_alerts / check_alerts_update look up WALLET00000001 / BTC.
    sqlx::query("UPDATE alerts SET wallet_address = 'WALLET00000001', underlying = 'BTC' WHERE id = 'a001'")
        .execute(pool).await.unwrap();

    sqlx::query(
        "INSERT INTO watchlist (wallet_address, underlying)
         WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM cnt WHERE x < 100)
         SELECT 'WALLET' || printf('%08d', (x % 100) + 1),
                CASE (x % 4) WHEN 0 THEN 'BTC' WHEN 1 THEN 'ETH' WHEN 2 THEN 'SOL' ELSE 'XLM' END
         FROM cnt",
    )
    .execute(pool).await.unwrap();

    sqlx::query(
        "INSERT INTO sessions (token, wallet_address, expires_at)
         WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM cnt WHERE x < 100)
         SELECT 'sometoken' || printf('%03d', x),
                'WALLET' || printf('%08d', (x % 100) + 1),
                '2099-01-01T00:00:00.000Z'
         FROM cnt",
    )
    .execute(pool).await.unwrap();

    sqlx::query(
        "INSERT INTO auth_nonces (nonce, wallet_address, expires_at)
         WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM cnt WHERE x < 100)
         SELECT 'somenonce' || printf('%03d', x),
                'WALLET' || printf('%08d', (x % 100) + 1),
                '2099-01-01T00:00:00.000Z'
         FROM cnt",
    )
    .execute(pool).await.unwrap();
}

#[tokio::test]
async fn smoke_query_plans_use_indexes() {
    let (pool, db_path) = seeded_db(100, 1_000, 0).await;

    for spec in perf::QUERY_CATALOGUE {
        let plan = perf::explain_plan(&pool, spec).await;
        perf::assert_plan(spec, &plan);
    }

    pool.close().await;
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn full_suite_query_plans_and_latency() {
    if !full_suite_enabled() {
        return;
    }

    let (pool, db_path) = seeded_db(10_000, 100_000, 1_000_000).await;

    for spec in perf::QUERY_CATALOGUE {
        let plan = perf::explain_plan(&pool, spec).await;
        perf::assert_plan(spec, &plan);

        let p95 = perf::p95_latency_us(&pool, spec, 20).await;
        assert!(
            p95 <= spec.p95_budget_us,
            "{}: p95 latency {p95}us exceeds budget {}us",
            spec.name,
            spec.p95_budget_us
        );
    }

    pool.close().await;
    let _ = std::fs::remove_file(&db_path);
}
