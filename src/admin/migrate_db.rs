use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{FromRow, PgPool, Postgres, SqlitePool, Transaction};
use time::{OffsetDateTime, UtcOffset};

use super::{admin_error, AdminResult};
use crate::models::{Account, Alert, Position, WatchlistItem};

#[derive(Debug, Clone, FromRow, Serialize)]
struct AuthNonceRow {
    nonce: String,
    wallet_address: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, FromRow, Serialize)]
struct SessionRow {
    token: String,
    wallet_address: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub(super) struct PriceTickRow {
    id: String,
    underlying: String,
    price: f64,
    #[serde(with = "time::serde::rfc3339")]
    observed_at: OffsetDateTime,
}

#[derive(Debug, Clone, FromRow, Serialize)]
struct SqlxMigrationRow {
    version: i64,
    description: String,
    #[serde(with = "time::serde::rfc3339")]
    installed_on: OffsetDateTime,
    success: bool,
    checksum: Vec<u8>,
    execution_time: i64,
}

pub(super) fn rows_checksum<T: Serialize>(rows: &[T]) -> AdminResult<String> {
    let mut hasher = Sha256::new();
    for row in rows {
        let encoded = serde_json::to_vec(row)?;
        hasher.update((encoded.len() as u64).to_be_bytes());
        hasher.update(encoded);
    }
    Ok(data_encoding::HEXLOWER.encode(&hasher.finalize()))
}

fn postgres_timestamp(value: OffsetDateTime) -> AdminResult<OffsetDateTime> {
    let utc = value.to_offset(UtcOffset::UTC);
    Ok(utc.replace_nanosecond((utc.nanosecond() / 1_000) * 1_000)?)
}

fn normalize_timestamps<T>(
    rows: &mut [T],
    mut normalize: impl FnMut(&mut T) -> AdminResult<()>,
) -> AdminResult<()> {
    for row in rows {
        normalize(row)?;
    }
    Ok(())
}

async fn guard_destination_table(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    resume: bool,
) -> AdminResult<()> {
    if resume {
        return Ok(());
    }
    let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(&mut **tx)
        .await?;
    if count != 0 {
        return Err(admin_error(format!(
            "destination table {table} is non-empty; pass --resume to continue"
        )));
    }
    Ok(())
}

async fn save_progress(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    row_count: i64,
    checksum: &str,
) -> AdminResult<()> {
    sqlx::query(
        "INSERT INTO _zenith_migration_progress (table_name, row_count, checksum)
         VALUES ($1, $2, $3)
         ON CONFLICT (table_name) DO UPDATE
             SET row_count = EXCLUDED.row_count,
                 checksum = EXCLUDED.checksum,
                 completed_at = now()",
    )
    .bind(table)
    .bind(row_count)
    .bind(checksum)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn migration_schema() -> [&'static str; 17] {
    [
        "CREATE TABLE IF NOT EXISTS accounts (wallet_address TEXT PRIMARY KEY, balance DOUBLE PRECISION NOT NULL DEFAULT 100000.0, collateral_locked DOUBLE PRECISION NOT NULL DEFAULT 0.0, created_at TIMESTAMPTZ NOT NULL DEFAULT now())",
        "CREATE TABLE IF NOT EXISTS positions (id TEXT PRIMARY KEY, wallet_address TEXT NOT NULL REFERENCES accounts(wallet_address), underlying TEXT NOT NULL, strike DOUBLE PRECISION NOT NULL, expiry_days DOUBLE PRECISION NOT NULL, option_type TEXT NOT NULL CHECK (option_type IN ('call', 'put')), position_type TEXT NOT NULL CHECK (position_type IN ('long', 'short')), contracts DOUBLE PRECISION NOT NULL, entry_premium DOUBLE PRECISION NOT NULL, entry_spot DOUBLE PRECISION NOT NULL, collateral DOUBLE PRECISION NOT NULL DEFAULT 0.0, status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'closed', 'rolled')), close_premium DOUBLE PRECISION, close_spot DOUBLE PRECISION, realized_pnl DOUBLE PRECISION, opened_at TIMESTAMPTZ NOT NULL DEFAULT now(), closed_at TIMESTAMPTZ, strategy_id TEXT)",
        "CREATE TABLE IF NOT EXISTS watchlist (wallet_address TEXT NOT NULL REFERENCES accounts(wallet_address), underlying TEXT NOT NULL, added_at TIMESTAMPTZ NOT NULL DEFAULT now(), PRIMARY KEY (wallet_address, underlying))",
        "CREATE TABLE IF NOT EXISTS alerts (id TEXT PRIMARY KEY, wallet_address TEXT NOT NULL REFERENCES accounts(wallet_address), underlying TEXT NOT NULL, condition TEXT NOT NULL CHECK (condition IN ('above', 'below')), target_price DOUBLE PRECISION NOT NULL, triggered BOOLEAN NOT NULL DEFAULT FALSE, created_at TIMESTAMPTZ NOT NULL DEFAULT now(), triggered_at TIMESTAMPTZ)",
        "CREATE TABLE IF NOT EXISTS auth_nonces (nonce TEXT PRIMARY KEY, wallet_address TEXT NOT NULL, created_at TIMESTAMPTZ NOT NULL DEFAULT now(), expires_at TIMESTAMPTZ NOT NULL)",
        "CREATE TABLE IF NOT EXISTS sessions (token TEXT PRIMARY KEY, wallet_address TEXT NOT NULL REFERENCES accounts(wallet_address), created_at TIMESTAMPTZ NOT NULL DEFAULT now(), expires_at TIMESTAMPTZ NOT NULL)",
        "CREATE TABLE IF NOT EXISTS price_ticks (id TEXT PRIMARY KEY, underlying TEXT NOT NULL, price DOUBLE PRECISION NOT NULL, observed_at TIMESTAMPTZ NOT NULL)",
        "CREATE TABLE IF NOT EXISTS _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TIMESTAMPTZ NOT NULL, success BOOLEAN NOT NULL, checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL)",
        "CREATE TABLE IF NOT EXISTS _zenith_migration_progress (table_name TEXT PRIMARY KEY, row_count BIGINT NOT NULL, checksum TEXT NOT NULL, completed_at TIMESTAMPTZ NOT NULL DEFAULT now())",
        "CREATE INDEX IF NOT EXISTS idx_positions_wallet ON positions(wallet_address)",
        "CREATE INDEX IF NOT EXISTS idx_positions_wallet_status ON positions(wallet_address, status)",
        "CREATE INDEX IF NOT EXISTS idx_positions_strategy ON positions(strategy_id) WHERE strategy_id IS NOT NULL",
        "CREATE INDEX IF NOT EXISTS idx_watchlist_wallet ON watchlist(wallet_address)",
        "CREATE INDEX IF NOT EXISTS idx_alerts_wallet ON alerts(wallet_address)",
        "CREATE INDEX IF NOT EXISTS idx_alerts_untriggered ON alerts(underlying, triggered) WHERE triggered = FALSE",
        "CREATE INDEX IF NOT EXISTS idx_sessions_wallet ON sessions(wallet_address)",
        "CREATE INDEX IF NOT EXISTS idx_price_ticks_symbol_time ON price_ticks(underlying, observed_at)",
    ]
}

async fn migrate_accounts(source: &SqlitePool, target: &PgPool, resume: bool) -> AdminResult<()> {
    let mut rows: Vec<Account> = sqlx::query_as("SELECT * FROM accounts ORDER BY wallet_address")
        .fetch_all(source)
        .await?;
    normalize_timestamps(&mut rows, |row| {
        row.created_at = postgres_timestamp(row.created_at)?;
        Ok(())
    })?;
    let checksum = rows_checksum(&rows)?;
    let mut tx = target.begin().await?;
    guard_destination_table(&mut tx, "accounts", resume).await?;
    for row in &rows {
        sqlx::query(
            "INSERT INTO accounts (wallet_address, balance, collateral_locked, created_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (wallet_address) DO UPDATE SET balance = EXCLUDED.balance,
                 collateral_locked = EXCLUDED.collateral_locked, created_at = EXCLUDED.created_at",
        )
        .bind(&row.wallet_address)
        .bind(row.balance)
        .bind(row.collateral_locked)
        .bind(row.created_at)
        .execute(&mut *tx)
        .await?;
    }
    let copied: Vec<Account> = sqlx::query_as("SELECT * FROM accounts ORDER BY wallet_address")
        .fetch_all(&mut *tx)
        .await?;
    verify_rows(&mut tx, "accounts", &rows, &copied, &checksum).await?;
    tx.commit().await?;
    Ok(())
}

async fn migrate_positions(source: &SqlitePool, target: &PgPool, resume: bool) -> AdminResult<()> {
    let mut rows: Vec<Position> = sqlx::query_as("SELECT * FROM positions ORDER BY id")
        .fetch_all(source)
        .await?;
    normalize_timestamps(&mut rows, |row| {
        row.opened_at = postgres_timestamp(row.opened_at)?;
        row.closed_at = row.closed_at.map(postgres_timestamp).transpose()?;
        Ok(())
    })?;
    let checksum = rows_checksum(&rows)?;
    let mut tx = target.begin().await?;
    guard_destination_table(&mut tx, "positions", resume).await?;
    for row in &rows {
        sqlx::query(
            "INSERT INTO positions
                (id, wallet_address, underlying, strike, expiry_days, option_type,
                 position_type, contracts, entry_premium, entry_spot, collateral,
                 status, close_premium, close_spot, realized_pnl, opened_at, closed_at,
                 strategy_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                     $15, $16, $17, $18)
             ON CONFLICT (id) DO UPDATE SET wallet_address = EXCLUDED.wallet_address,
                 underlying = EXCLUDED.underlying, strike = EXCLUDED.strike,
                 expiry_days = EXCLUDED.expiry_days, option_type = EXCLUDED.option_type,
                 position_type = EXCLUDED.position_type, contracts = EXCLUDED.contracts,
                 entry_premium = EXCLUDED.entry_premium, entry_spot = EXCLUDED.entry_spot,
                 collateral = EXCLUDED.collateral, status = EXCLUDED.status,
                 close_premium = EXCLUDED.close_premium, close_spot = EXCLUDED.close_spot,
                 realized_pnl = EXCLUDED.realized_pnl, opened_at = EXCLUDED.opened_at,
                 closed_at = EXCLUDED.closed_at, strategy_id = EXCLUDED.strategy_id",
        )
        .bind(&row.id)
        .bind(&row.wallet_address)
        .bind(&row.underlying)
        .bind(row.strike)
        .bind(row.expiry_days)
        .bind(&row.option_type)
        .bind(&row.position_type)
        .bind(row.contracts)
        .bind(row.entry_premium)
        .bind(row.entry_spot)
        .bind(row.collateral)
        .bind(&row.status)
        .bind(row.close_premium)
        .bind(row.close_spot)
        .bind(row.realized_pnl)
        .bind(row.opened_at)
        .bind(row.closed_at)
        .bind(&row.strategy_id)
        .execute(&mut *tx)
        .await?;
    }
    let copied: Vec<Position> = sqlx::query_as("SELECT * FROM positions ORDER BY id")
        .fetch_all(&mut *tx)
        .await?;
    verify_rows(&mut tx, "positions", &rows, &copied, &checksum).await?;
    tx.commit().await?;
    Ok(())
}

async fn migrate_watchlist(source: &SqlitePool, target: &PgPool, resume: bool) -> AdminResult<()> {
    let mut rows: Vec<WatchlistItem> =
        sqlx::query_as("SELECT * FROM watchlist ORDER BY wallet_address, underlying")
            .fetch_all(source)
            .await?;
    normalize_timestamps(&mut rows, |row| {
        row.added_at = postgres_timestamp(row.added_at)?;
        Ok(())
    })?;
    let checksum = rows_checksum(&rows)?;
    let mut tx = target.begin().await?;
    guard_destination_table(&mut tx, "watchlist", resume).await?;
    for row in &rows {
        sqlx::query(
            "INSERT INTO watchlist (wallet_address, underlying, added_at) VALUES ($1, $2, $3)
             ON CONFLICT (wallet_address, underlying) DO UPDATE SET added_at = EXCLUDED.added_at",
        )
        .bind(&row.wallet_address)
        .bind(&row.underlying)
        .bind(row.added_at)
        .execute(&mut *tx)
        .await?;
    }
    let copied: Vec<WatchlistItem> =
        sqlx::query_as("SELECT * FROM watchlist ORDER BY wallet_address, underlying")
            .fetch_all(&mut *tx)
            .await?;
    verify_rows(&mut tx, "watchlist", &rows, &copied, &checksum).await?;
    tx.commit().await?;
    Ok(())
}

async fn migrate_alerts(source: &SqlitePool, target: &PgPool, resume: bool) -> AdminResult<()> {
    let mut rows: Vec<Alert> = sqlx::query_as("SELECT * FROM alerts ORDER BY id")
        .fetch_all(source)
        .await?;
    normalize_timestamps(&mut rows, |row| {
        row.created_at = postgres_timestamp(row.created_at)?;
        row.triggered_at = row.triggered_at.map(postgres_timestamp).transpose()?;
        Ok(())
    })?;
    let checksum = rows_checksum(&rows)?;
    let mut tx = target.begin().await?;
    guard_destination_table(&mut tx, "alerts", resume).await?;
    for row in &rows {
        sqlx::query(
            "INSERT INTO alerts
                (id, wallet_address, underlying, condition, target_price, triggered,
                 created_at, triggered_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (id) DO UPDATE SET wallet_address = EXCLUDED.wallet_address,
                 underlying = EXCLUDED.underlying, condition = EXCLUDED.condition,
                 target_price = EXCLUDED.target_price, triggered = EXCLUDED.triggered,
                 created_at = EXCLUDED.created_at, triggered_at = EXCLUDED.triggered_at",
        )
        .bind(&row.id)
        .bind(&row.wallet_address)
        .bind(&row.underlying)
        .bind(&row.condition)
        .bind(row.target_price)
        .bind(row.triggered)
        .bind(row.created_at)
        .bind(row.triggered_at)
        .execute(&mut *tx)
        .await?;
    }
    let copied: Vec<Alert> = sqlx::query_as("SELECT * FROM alerts ORDER BY id")
        .fetch_all(&mut *tx)
        .await?;
    verify_rows(&mut tx, "alerts", &rows, &copied, &checksum).await?;
    tx.commit().await?;
    Ok(())
}

async fn migrate_auth_rows(source: &SqlitePool, target: &PgPool, resume: bool) -> AdminResult<()> {
    let mut nonces: Vec<AuthNonceRow> = sqlx::query_as("SELECT * FROM auth_nonces ORDER BY nonce")
        .fetch_all(source)
        .await?;
    normalize_timestamps(&mut nonces, |row| {
        row.created_at = postgres_timestamp(row.created_at)?;
        row.expires_at = postgres_timestamp(row.expires_at)?;
        Ok(())
    })?;
    let nonce_checksum = rows_checksum(&nonces)?;
    let mut tx = target.begin().await?;
    guard_destination_table(&mut tx, "auth_nonces", resume).await?;
    for row in &nonces {
        sqlx::query(
            "INSERT INTO auth_nonces (nonce, wallet_address, created_at, expires_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (nonce) DO UPDATE SET wallet_address = EXCLUDED.wallet_address,
                 created_at = EXCLUDED.created_at, expires_at = EXCLUDED.expires_at",
        )
        .bind(&row.nonce)
        .bind(&row.wallet_address)
        .bind(row.created_at)
        .bind(row.expires_at)
        .execute(&mut *tx)
        .await?;
    }
    let copied_nonces: Vec<AuthNonceRow> =
        sqlx::query_as("SELECT * FROM auth_nonces ORDER BY nonce")
            .fetch_all(&mut *tx)
            .await?;
    verify_rows(
        &mut tx,
        "auth_nonces",
        &nonces,
        &copied_nonces,
        &nonce_checksum,
    )
    .await?;
    tx.commit().await?;

    let mut sessions: Vec<SessionRow> = sqlx::query_as("SELECT * FROM sessions ORDER BY token")
        .fetch_all(source)
        .await?;
    normalize_timestamps(&mut sessions, |row| {
        row.created_at = postgres_timestamp(row.created_at)?;
        row.expires_at = postgres_timestamp(row.expires_at)?;
        Ok(())
    })?;
    let session_checksum = rows_checksum(&sessions)?;
    let mut tx = target.begin().await?;
    guard_destination_table(&mut tx, "sessions", resume).await?;
    for row in &sessions {
        sqlx::query(
            "INSERT INTO sessions (token, wallet_address, created_at, expires_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (token) DO UPDATE SET wallet_address = EXCLUDED.wallet_address,
                 created_at = EXCLUDED.created_at, expires_at = EXCLUDED.expires_at",
        )
        .bind(&row.token)
        .bind(&row.wallet_address)
        .bind(row.created_at)
        .bind(row.expires_at)
        .execute(&mut *tx)
        .await?;
    }
    let copied_sessions: Vec<SessionRow> = sqlx::query_as("SELECT * FROM sessions ORDER BY token")
        .fetch_all(&mut *tx)
        .await?;
    verify_rows(
        &mut tx,
        "sessions",
        &sessions,
        &copied_sessions,
        &session_checksum,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn migrate_ticks(source: &SqlitePool, target: &PgPool, resume: bool) -> AdminResult<()> {
    let mut rows: Vec<PriceTickRow> = sqlx::query_as("SELECT * FROM price_ticks ORDER BY id")
        .fetch_all(source)
        .await?;
    normalize_timestamps(&mut rows, |row| {
        row.observed_at = postgres_timestamp(row.observed_at)?;
        Ok(())
    })?;
    let checksum = rows_checksum(&rows)?;
    let mut tx = target.begin().await?;
    guard_destination_table(&mut tx, "price_ticks", resume).await?;
    for row in &rows {
        sqlx::query(
            "INSERT INTO price_ticks (id, underlying, price, observed_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET underlying = EXCLUDED.underlying,
                 price = EXCLUDED.price, observed_at = EXCLUDED.observed_at",
        )
        .bind(&row.id)
        .bind(&row.underlying)
        .bind(row.price)
        .bind(row.observed_at)
        .execute(&mut *tx)
        .await?;
    }
    let copied: Vec<PriceTickRow> = sqlx::query_as("SELECT * FROM price_ticks ORDER BY id")
        .fetch_all(&mut *tx)
        .await?;
    verify_rows(&mut tx, "price_ticks", &rows, &copied, &checksum).await?;
    tx.commit().await?;
    Ok(())
}

async fn migrate_sqlx_history(
    source: &SqlitePool,
    target: &PgPool,
    resume: bool,
) -> AdminResult<()> {
    let mut rows: Vec<SqlxMigrationRow> =
        sqlx::query_as("SELECT * FROM _sqlx_migrations ORDER BY version")
            .fetch_all(source)
            .await?;
    normalize_timestamps(&mut rows, |row| {
        row.installed_on = postgres_timestamp(row.installed_on)?;
        Ok(())
    })?;
    let checksum = rows_checksum(&rows)?;
    let mut tx = target.begin().await?;
    guard_destination_table(&mut tx, "_sqlx_migrations", resume).await?;
    for row in &rows {
        sqlx::query(
            "INSERT INTO _sqlx_migrations
                (version, description, installed_on, success, checksum, execution_time)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (version) DO UPDATE SET description = EXCLUDED.description,
                 installed_on = EXCLUDED.installed_on, success = EXCLUDED.success,
                 checksum = EXCLUDED.checksum, execution_time = EXCLUDED.execution_time",
        )
        .bind(row.version)
        .bind(&row.description)
        .bind(row.installed_on)
        .bind(row.success)
        .bind(&row.checksum)
        .bind(row.execution_time)
        .execute(&mut *tx)
        .await?;
    }
    let copied: Vec<SqlxMigrationRow> =
        sqlx::query_as("SELECT * FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut *tx)
            .await?;
    verify_rows(&mut tx, "_sqlx_migrations", &rows, &copied, &checksum).await?;
    tx.commit().await?;
    Ok(())
}

async fn verify_rows<T: Serialize>(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    source: &[T],
    copied: &[T],
    source_checksum: &str,
) -> AdminResult<()> {
    let target_checksum = rows_checksum(copied)?;
    if source.len() != copied.len() || target_checksum != source_checksum {
        return Err(admin_error(format!(
            "verification failed for {table}: source rows/checksum {}:{source_checksum}, target {}:{target_checksum}",
            source.len(),
            copied.len()
        )));
    }
    save_progress(tx, table, source.len() as i64, source_checksum).await?;
    println!(
        "verified {table}: {} rows, SHA-256 {source_checksum}",
        source.len()
    );
    Ok(())
}

/// Copies a current SQLite schema to PostgreSQL in foreign-key dependency
/// order. Each table is committed only after row counts and canonical
/// SHA-256 checksums match, so rerunning with `resume` safely replays
/// incomplete tables using conflict-safe upserts.
pub async fn migrate_database(from: &str, to: &str, resume: bool) -> AdminResult<()> {
    if !from.starts_with("sqlite://") {
        return Err(admin_error("migration source must use sqlite://"));
    }
    if !(to.starts_with("postgres://") || to.starts_with("postgresql://")) {
        return Err(admin_error("migration destination must use postgres://"));
    }
    let source = SqlitePoolOptions::new()
        .max_connections(2)
        .connect(from)
        .await?;
    let source_tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )
    .fetch_all(&source)
    .await?;
    let expected_tables = [
        "_sqlx_migrations",
        "accounts",
        "alerts",
        "auth_nonces",
        "positions",
        "price_ticks",
        "sessions",
        "watchlist",
    ];
    let actual_tables: Vec<_> = source_tables.iter().map(String::as_str).collect();
    if actual_tables != expected_tables {
        return Err(admin_error(format!(
            "SQLite schema does not match the supported backend schema: found {actual_tables:?}"
        )));
    }
    let target = PgPoolOptions::new().max_connections(5).connect(to).await?;
    for statement in migration_schema() {
        sqlx::query(statement).execute(&target).await?;
    }

    migrate_accounts(&source, &target, resume).await?;
    migrate_positions(&source, &target, resume).await?;
    migrate_watchlist(&source, &target, resume).await?;
    migrate_alerts(&source, &target, resume).await?;
    migrate_auth_rows(&source, &target, resume).await?;
    migrate_ticks(&source, &target, resume).await?;
    migrate_sqlx_history(&source, &target, resume).await?;

    source.close().await;
    target.close().await;
    println!("All SQLite tables copied and verified.");
    Ok(())
}
