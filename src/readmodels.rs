use serde::Serialize;
use sqlx::sqlite::Sqlite;
use sqlx::{FromRow, SqlitePool, Transaction};

use crate::models::Position;

/// One row per multi-leg strategy, maintained incrementally by
/// `apply_position_opened`/`apply_position_closed` inside the same
/// transaction as the source mutation. `list_strategies` reads these rows
/// instead of scanning and aggregating every leg on every request;
/// unrealized P&L is still computed live, but only over open legs.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct StrategySummaryRow {
    pub strategy_id: String,
    pub wallet_address: String,
    pub underlying: String,
    pub leg_count: i64,
    pub open_leg_count: i64,
    pub status: String,
    pub opened_at: String,
    pub realized_pnl: f64,
}

/// Per-wallet realized-trade aggregates, maintained incrementally by
/// `apply_position_closed`. The history stats endpoint reads this single
/// row instead of aggregating every closed position on every request.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct WalletCountsRow {
    pub wallet_address: String,
    pub trade_count: i64,
    pub win_count: i64,
    pub loss_count: i64,
    pub total_realized_pnl: f64,
}

/// Applies the read-model side effects of a position being opened (or
/// re-opened as a roll replacement). Must run inside the same transaction
/// as the position insert. Single-leg positions (strategy_id NULL) don't
/// touch the strategy summary; opening never changes the realized-trade
/// counts, since those only move when a position settles.
pub async fn apply_position_opened(
    tx: &mut Transaction<'_, Sqlite>,
    position: &Position,
) -> Result<(), sqlx::Error> {
    let Some(strategy_id) = &position.strategy_id else {
        return Ok(());
    };

    // A roll's replacement leg re-opens an existing strategy row: leg_count
    // and open_leg_count go up by one and the strategy is 'open' again. The
    // underlying/opened_at/wallet_address captured on first insert are kept
    // (they describe the strategy's original legs), so the ON CONFLICT clause
    // deliberately doesn't overwrite them.
    sqlx::query(
        "INSERT INTO strategy_summaries
            (strategy_id, wallet_address, underlying, leg_count, open_leg_count, status, opened_at, realized_pnl)
         VALUES (?, ?, ?, 1, 1, 'open', ?, 0.0)
         ON CONFLICT(strategy_id) DO UPDATE SET
            leg_count = leg_count + 1,
            open_leg_count = open_leg_count + 1,
            status = 'open'",
    )
    .bind(strategy_id)
    .bind(&position.wallet_address)
    .bind(&position.underlying)
    .bind(&position.opened_at)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Applies the read-model side effects of a position being closed (or
/// rolled, which settles the old leg before opening its replacement).
/// Must run inside the same transaction as the position update.
pub async fn apply_position_closed(
    tx: &mut Transaction<'_, Sqlite>,
    position: &Position,
) -> Result<(), sqlx::Error> {
    if let Some(strategy_id) = &position.strategy_id {
        sqlx::query(
            "UPDATE strategy_summaries
                SET open_leg_count = open_leg_count - 1,
                    realized_pnl = realized_pnl + ?,
                    status = CASE WHEN open_leg_count - 1 > 0 THEN 'open' ELSE 'closed' END
              WHERE strategy_id = ?",
        )
        .bind(position.realized_pnl.unwrap_or(0.0))
        .bind(strategy_id)
        .execute(&mut **tx)
        .await?;
    }

    // Every settled position counts as a trade, strategy leg or not — the
    // history stats have always covered all closed/rolled rows. A break-even
    // trade (realized_pnl = 0) is neither a win nor a loss, matching the
    // old SUM(CASE WHEN realized_pnl > 0 ...) aggregation.
    let realized = position.realized_pnl.unwrap_or(0.0);
    let (win, loss) = (realized > 0.0, realized < 0.0);
    sqlx::query(
        "INSERT INTO wallet_position_counts
            (wallet_address, trade_count, win_count, loss_count, total_realized_pnl)
         VALUES (?, 1, ?, ?, ?)
         ON CONFLICT(wallet_address) DO UPDATE SET
            trade_count = trade_count + 1,
            win_count = win_count + excluded.win_count,
            loss_count = loss_count + excluded.loss_count,
            total_realized_pnl = total_realized_pnl + excluded.total_realized_pnl",
    )
    .bind(&position.wallet_address)
    .bind(win)
    .bind(loss)
    .bind(realized)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Rebuilds both read-model tables from the positions table. Used by
/// `zenith-admin readmodels rebuild` for recovery after drift or a restore.
/// The whole rebuild runs in one transaction so the tables never appear
/// half-populated to a concurrent reader.
pub async fn rebuild_all(db: &SqlitePool) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;

    sqlx::query("DELETE FROM strategy_summaries")
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM wallet_position_counts")
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "INSERT INTO strategy_summaries
            (strategy_id, wallet_address, underlying, leg_count, open_leg_count, status, opened_at, realized_pnl)
         SELECT p.strategy_id,
                MIN(p.wallet_address),
                MIN(p.underlying),
                COUNT(*),
                SUM(p.status = 'open'),
                CASE WHEN SUM(p.status = 'open') > 0 THEN 'open' ELSE 'closed' END,
                MIN(p.opened_at),
                SUM(COALESCE(p.realized_pnl, 0.0))
           FROM positions p
          WHERE p.strategy_id IS NOT NULL
          GROUP BY p.strategy_id",
    )
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO wallet_position_counts
            (wallet_address, trade_count, win_count, loss_count, total_realized_pnl)
         SELECT wallet_address,
                COUNT(*),
                SUM(realized_pnl > 0),
                SUM(realized_pnl < 0),
                SUM(realized_pnl)
           FROM positions
          WHERE status IN ('closed', 'rolled')
          GROUP BY wallet_address",
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Compares the read models against a fresh aggregation over the positions
/// table and returns a human-readable description of every discrepancy
/// (empty when they agree). Runs in read-only queries; the consistency
/// checker job calls this on an interval and logs anything it finds.
pub async fn check_consistency(db: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
    let mut problems = Vec::new();

    // ── strategy_summaries ────────────────────────────────────────────────
    let stored: Vec<StrategySummaryRow> =
        sqlx::query_as("SELECT * FROM strategy_summaries")
            .fetch_all(db)
            .await?;
    #[derive(FromRow)]
    struct ComputedStrategy {
        strategy_id: String,
        leg_count: i64,
        open_leg_count: i64,
        opened_at: String,
        realized_pnl: f64,
    }
    let computed: Vec<ComputedStrategy> = sqlx::query_as(
        "SELECT strategy_id,
                COUNT(*) AS leg_count,
                SUM(status = 'open') AS open_leg_count,
                MIN(opened_at) AS opened_at,
                SUM(COALESCE(realized_pnl, 0.0)) AS realized_pnl
           FROM positions
          WHERE strategy_id IS NOT NULL
          GROUP BY strategy_id",
    )
    .fetch_all(db)
    .await?;

    let mut computed_by_id: std::collections::HashMap<&str, &ComputedStrategy> =
        std::collections::HashMap::new();
    for c in &computed {
        computed_by_id.insert(c.strategy_id.as_str(), c);
    }
    for s in &stored {
        match computed_by_id.remove(s.strategy_id.as_str()) {
            None => problems.push(format!(
                "strategy_summaries: row for strategy {} has no legs in positions",
                s.strategy_id
            )),
            Some(c) => {
                if s.leg_count != c.leg_count {
                    problems.push(format!(
                        "strategy_summaries: strategy {} leg_count {} != {}",
                        s.strategy_id, s.leg_count, c.leg_count
                    ));
                }
                if s.open_leg_count != c.open_leg_count {
                    problems.push(format!(
                        "strategy_summaries: strategy {} open_leg_count {} != {}",
                        s.strategy_id, s.open_leg_count, c.open_leg_count
                    ));
                }
                if s.opened_at != c.opened_at {
                    problems.push(format!(
                        "strategy_summaries: strategy {} opened_at {} != {}",
                        s.strategy_id, s.opened_at, c.opened_at
                    ));
                }
                if (s.realized_pnl - c.realized_pnl).abs() > 0.01 {
                    problems.push(format!(
                        "strategy_summaries: strategy {} realized_pnl {} != {}",
                        s.strategy_id, s.realized_pnl, c.realized_pnl
                    ));
                }
            }
        }
    }
    for c in computed_by_id.values() {
        problems.push(format!(
            "strategy_summaries: missing row for strategy {} ({} legs)",
            c.strategy_id, c.leg_count
        ));
    }

    // ── wallet_position_counts ────────────────────────────────────────────
    let stored_counts: Vec<WalletCountsRow> =
        sqlx::query_as("SELECT * FROM wallet_position_counts")
            .fetch_all(db)
            .await?;
    #[derive(FromRow)]
    struct ComputedCounts {
        wallet_address: String,
        trade_count: i64,
        win_count: i64,
        loss_count: i64,
        total_realized_pnl: f64,
    }
    let computed_counts: Vec<ComputedCounts> = sqlx::query_as(
        "SELECT wallet_address,
                COUNT(*) AS trade_count,
                SUM(realized_pnl > 0) AS win_count,
                SUM(realized_pnl < 0) AS loss_count,
                SUM(realized_pnl) AS total_realized_pnl
           FROM positions
          WHERE status IN ('closed', 'rolled')
          GROUP BY wallet_address",
    )
    .fetch_all(db)
    .await?;

    let mut computed_by_wallet: std::collections::HashMap<&str, &ComputedCounts> =
        std::collections::HashMap::new();
    for c in &computed_counts {
        computed_by_wallet.insert(c.wallet_address.as_str(), c);
    }
    for s in &stored_counts {
        match computed_by_wallet.remove(s.wallet_address.as_str()) {
            None => problems.push(format!(
                "wallet_position_counts: row for wallet {} has no settled positions",
                s.wallet_address
            )),
            Some(c) => {
                if s.trade_count != c.trade_count
                    || s.win_count != c.win_count
                    || s.loss_count != c.loss_count
                    || (s.total_realized_pnl - c.total_realized_pnl).abs() > 0.01
                {
                    problems.push(format!(
                        "wallet_position_counts: wallet {} stored ({} trades, {}W/{}L, pnl {}) != computed ({} trades, {}W/{}L, pnl {})",
                        s.wallet_address,
                        s.trade_count,
                        s.win_count,
                        s.loss_count,
                        s.total_realized_pnl,
                        c.trade_count,
                        c.win_count,
                        c.loss_count,
                        c.total_realized_pnl,
                    ));
                }
            }
        }
    }
    for c in computed_by_wallet.values() {
        problems.push(format!(
            "wallet_position_counts: missing row for wallet {} ({} trades)",
            c.wallet_address, c.trade_count
        ));
    }

    Ok(problems)
}

/// Serving query for `list_strategies`: one row per strategy, newest first.
pub async fn strategy_summaries_for_wallet(
    db: &SqlitePool,
    wallet: &str,
) -> Result<Vec<StrategySummaryRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT * FROM strategy_summaries
          WHERE wallet_address = ?
         ORDER BY opened_at DESC",
    )
    .bind(wallet)
    .fetch_all(db)
    .await
}

/// Serving query for the history stats: the wallet's single aggregate row,
/// or None before the wallet has settled its first position.
pub async fn wallet_position_counts(
    db: &SqlitePool,
    wallet: &str,
) -> Result<Option<WalletCountsRow>, sqlx::Error> {
    sqlx::query_as("SELECT * FROM wallet_position_counts WHERE wallet_address = ?")
        .bind(wallet)
        .fetch_optional(db)
        .await
}

/// Background consistency checker: recomputes the read models from source
/// every 10 minutes and logs any drift found. Drift is reported, not
/// auto-repaired — `zenith-admin readmodels rebuild` is the recovery path,
/// so a bug in the incremental maintenance can't silently "fix" itself by
/// rebuilding from data the same bug corrupted.
pub async fn check_consistency_loop(db: SqlitePool) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10 * 60));
    loop {
        interval.tick().await;
        match check_consistency(&db).await {
            Ok(problems) => {
                for problem in &problems {
                    tracing::warn!(problem, "read model consistency check found drift");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "read model consistency check failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> (SqlitePool, std::path::PathBuf) {
        let db_path = std::env::temp_dir().join(format!(
            "zenith-readmodels-test-{}.db",
            uuid::Uuid::new_v4()
        ));
        let pool = crate::db::init_pool(&format!("sqlite://{}", db_path.display())).await;
        (pool, db_path)
    }

    async fn insert_position(
        db: &SqlitePool,
        id: &str,
        wallet: &str,
        status: &str,
        strategy_id: Option<&str>,
        realized_pnl: Option<f64>,
    ) -> Position {
        sqlx::query(
            "INSERT INTO positions
                (id, wallet_address, underlying, strike, expiry_days, option_type,
                 position_type, contracts, entry_premium, entry_spot, status, realized_pnl, strategy_id)
             VALUES (?, ?, 'BTC', 70000, 30, 'call', 'long', 1, 100, 67000, ?, ?, ?)",
        )
        .bind(id)
        .bind(wallet)
        .bind(status)
        .bind(realized_pnl)
        .bind(strategy_id)
        .execute(db)
        .await
        .unwrap();
        sqlx::query_as("SELECT * FROM positions WHERE id = ?")
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn open_then_close_maintains_both_read_models() {
        let (db, db_path) = test_db().await;
        sqlx::query("INSERT INTO accounts (wallet_address) VALUES ('W')")
            .execute(&db)
            .await
            .unwrap();

        let mut tx = db.begin().await.unwrap();
        let opened = insert_position(&db, "p1", "W", "open", Some("s1"), None).await;
        apply_position_opened(&mut tx, &opened).await.unwrap();
        tx.commit().await.unwrap();

        let summary: StrategySummaryRow = sqlx::query_as(
            "SELECT * FROM strategy_summaries WHERE strategy_id = 's1'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(summary.leg_count, 1);
        assert_eq!(summary.open_leg_count, 1);
        assert_eq!(summary.status, "open");
        assert_eq!(summary.realized_pnl, 0.0);

        // No trade has settled yet, so the wallet counts row must not exist.
        assert!(wallet_position_counts(&db, "W").await.unwrap().is_none());

        let mut tx = db.begin().await.unwrap();
        let mut closed = opened.clone();
        closed.status = "closed".to_string();
        closed.realized_pnl = Some(42.0);
        apply_position_closed(&mut tx, &closed).await.unwrap();
        tx.commit().await.unwrap();

        let summary: StrategySummaryRow = sqlx::query_as(
            "SELECT * FROM strategy_summaries WHERE strategy_id = 's1'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(summary.open_leg_count, 0);
        assert_eq!(summary.status, "closed");
        assert_eq!(summary.realized_pnl, 42.0);

        let counts = wallet_position_counts(&db, "W").await.unwrap().unwrap();
        assert_eq!(counts.trade_count, 1);
        assert_eq!(counts.win_count, 1);
        assert_eq!(counts.loss_count, 0);
        assert_eq!(counts.total_realized_pnl, 42.0);

        db.close().await;
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn a_break_even_trade_is_neither_a_win_nor_a_loss() {
        let (db, db_path) = test_db().await;
        sqlx::query("INSERT INTO accounts (wallet_address) VALUES ('W')")
            .execute(&db)
            .await
            .unwrap();

        let mut tx = db.begin().await.unwrap();
        let closed = insert_position(&db, "p1", "W", "closed", None, Some(0.0)).await;
        apply_position_closed(&mut tx, &closed).await.unwrap();
        tx.commit().await.unwrap();

        let counts = wallet_position_counts(&db, "W").await.unwrap().unwrap();
        assert_eq!(counts.trade_count, 1);
        assert_eq!(counts.win_count, 0);
        assert_eq!(counts.loss_count, 0);

        db.close().await;
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn rebuild_all_reproduces_the_incremental_state() {
        let (db, db_path) = test_db().await;
        sqlx::query("INSERT INTO accounts (wallet_address) VALUES ('W')")
            .execute(&db)
            .await
            .unwrap();

        // Two strategies: s1 fully settled, s2 with one open leg. Plus a
        // plain single-leg closed position.
        let mut tx = db.begin().await.unwrap();
        let p1 = insert_position(&db, "p1", "W", "closed", Some("s1"), Some(10.0)).await;
        apply_position_opened(&mut tx, &p1).await.unwrap();
        apply_position_closed(&mut tx, &p1).await.unwrap();
        let p2 = insert_position(&db, "p2", "W", "open", Some("s2"), None).await;
        apply_position_opened(&mut tx, &p2).await.unwrap();
        let p3 = insert_position(&db, "p3", "W", "closed", None, Some(-5.0)).await;
        apply_position_opened(&mut tx, &p3).await.unwrap();
        apply_position_closed(&mut tx, &p3).await.unwrap();
        tx.commit().await.unwrap();

        // Wipe the read models and rebuild from source, as the recovery
        // command does.
        sqlx::query("DELETE FROM strategy_summaries")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("DELETE FROM wallet_position_counts")
            .execute(&db)
            .await
            .unwrap();
        rebuild_all(&db).await.unwrap();

        let problems = check_consistency(&db).await.unwrap();
        assert!(problems.is_empty(), "rebuild left drift: {problems:?}");

        let summaries = strategy_summaries_for_wallet(&db, "W").await.unwrap();
        assert_eq!(summaries.len(), 2);
        let s1 = summaries.iter().find(|s| s.strategy_id == "s1").unwrap();
        assert_eq!(s1.status, "closed");
        assert_eq!(s1.realized_pnl, 10.0);
        let s2 = summaries.iter().find(|s| s.strategy_id == "s2").unwrap();
        assert_eq!(s2.status, "open");
        assert_eq!(s2.open_leg_count, 1);

        let counts = wallet_position_counts(&db, "W").await.unwrap().unwrap();
        assert_eq!(counts.trade_count, 2);
        assert_eq!(counts.win_count, 1);
        assert_eq!(counts.loss_count, 1);
        assert_eq!(counts.total_realized_pnl, 5.0);

        db.close().await;
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn check_consistency_reports_injected_drift() {
        let (db, db_path) = test_db().await;
        sqlx::query("INSERT INTO accounts (wallet_address) VALUES ('W')")
            .execute(&db)
            .await
            .unwrap();
        rebuild_all(&db).await.unwrap();

        // Simulate drift: a lost update to the wallet counts.
        sqlx::query(
            "INSERT INTO wallet_position_counts
                (wallet_address, trade_count, win_count, loss_count, total_realized_pnl)
             VALUES ('W', 7, 7, 0, 700.0)",
        )
        .execute(&db)
        .await
        .unwrap();

        let problems = check_consistency(&db).await.unwrap();
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("wallet_position_counts"));
        assert!(problems[0].contains('W'));

        db.close().await;
        let _ = std::fs::remove_file(&db_path);
    }
}
