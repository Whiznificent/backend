use std::error::Error;
use std::io;
use std::path::Path;
use std::str::FromStr;

use axum::extract::{Path as AxumPath, State};
use rand::rngs::StdRng;
use rand::{Rng, RngCore, SeedableRng};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
#[cfg(test)]
use sqlx::FromRow;
use sqlx::{Connection, Row, SqliteConnection, SqlitePool};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::alerts::{self, CreateAlertRequest};
use crate::auth::AuthUser;
use crate::error::AppJson;
#[cfg(test)]
use crate::models::{Account, Alert, Position, WatchlistItem};
use crate::positions::{self, OpenPositionRequest, RollPositionRequest};
use crate::strategies::{self, ExecuteStrategyRequest};
use crate::AppState;

type AdminResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug, Clone, FromRow, Serialize)]
#[cfg(test)]
struct PriceTickRow {
    id: String,
    underlying: String,
    price: f64,
    #[serde(with = "time::serde::rfc3339")]
    observed_at: OffsetDateTime,
}

#[cfg(test)]
fn rows_checksum<T: Serialize>(rows: &[T]) -> AdminResult<String> {
    let mut hasher = Sha256::new();
    for row in rows {
        let encoded = serde_json::to_vec(row)?;
        hasher.update((encoded.len() as u64).to_be_bytes());
        hasher.update(encoded);
    }
    Ok(data_encoding::HEXLOWER.encode(&hasher.finalize()))
}

#[derive(Debug, PartialEq, Serialize)]
pub struct FixtureSummary {
    pub wallets: usize,
    pub strategies: usize,
    pub positions: usize,
    pub rolls: usize,
    pub closed_positions: usize,
    pub alerts: usize,
    pub ticks: usize,
}

fn admin_error(message: impl Into<String>) -> Box<dyn Error> {
    io::Error::other(message.into()).into()
}

fn api_result<T>(result: Result<T, crate::error::AppError>) -> AdminResult<T> {
    result.map_err(|error| admin_error(error.message))
}

fn request_for_leg(
    rng: &mut StdRng,
    underlying: &str,
    strike: f64,
    option_type: &str,
) -> OpenPositionRequest {
    OpenPositionRequest {
        underlying: underlying.to_string(),
        strike,
        expiry_days: rng.gen_range(7.0..=90.0),
        option_type: option_type.to_string(),
        position_type: if rng.gen_ratio(1, 3) {
            "short".to_string()
        } else {
            "long".to_string()
        },
        contracts: rng.gen_range(0.01..=0.05),
    }
}

async fn canonicalize_position_id(db: &SqlitePool, old_id: &str, new_id: &str) -> AdminResult<()> {
    sqlx::query("UPDATE positions SET id = ? WHERE id = ?")
        .bind(new_id)
        .bind(old_id)
        .execute(db)
        .await?;
    Ok(())
}

async fn stamp_position(
    db: &SqlitePool,
    id: &str,
    opened_at: OffsetDateTime,
    closed_at: Option<OffsetDateTime>,
) -> AdminResult<()> {
    sqlx::query("UPDATE positions SET opened_at = ?, closed_at = ? WHERE id = ?")
        .bind(opened_at)
        .bind(closed_at)
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

async fn stamp_strategy(
    db: &SqlitePool,
    strategy_id: &str,
    opened_at: OffsetDateTime,
) -> AdminResult<()> {
    sqlx::query(
        "UPDATE positions
            SET opened_at = ?,
                closed_at = CASE WHEN status = 'open' THEN NULL ELSE ? END
          WHERE strategy_id = ?",
    )
    .bind(opened_at)
    .bind(opened_at + Duration::hours(3))
    .bind(strategy_id)
    .execute(db)
    .await?;
    Ok(())
}

/// Creates an empty, deterministic paper-trading dataset by invoking the
/// same position, strategy, roll, alert, and price-tick logic used by the
/// API. Existing databases are never cleared or overwritten.
pub async fn generate_fixtures(
    db: SqlitePool,
    wallet_count: usize,
    day_count: usize,
    seed: u64,
) -> AdminResult<FixtureSummary> {
    if wallet_count == 0 || day_count == 0 || wallet_count > 1_000 || day_count > 3_650 {
        return Err(admin_error(
            "wallets and days must be positive (maximum 1,000 wallets and 3,650 days)",
        ));
    }
    if wallet_count.saturating_mul(day_count) > 20_000 {
        return Err(admin_error("wallets * days must not exceed 20,000"));
    }

    for table in [
        "accounts",
        "positions",
        "alerts",
        "watchlist",
        "price_ticks",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&db)
            .await?;
        if count > 0 {
            return Err(admin_error(format!(
                "refusing to generate fixtures: table {table} is not empty"
            )));
        }
    }

    let mut rng = StdRng::seed_from_u64(seed);
    let mut wallets = Vec::with_capacity(wallet_count);
    for _ in 0..wallet_count {
        let mut key_bytes = [0u8; 32];
        rng.fill_bytes(&mut key_bytes);
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&key_bytes);
        let wallet =
            crate::strkey::encode_stellar_public_key(signing_key.verifying_key().as_bytes());
        sqlx::query("INSERT INTO accounts (wallet_address) VALUES (?)")
            .bind(&wallet)
            .execute(&db)
            .await?;
        wallets.push(wallet);
    }

    let anchor = OffsetDateTime::parse("2026-01-01T00:00:00Z", &Rfc3339)?
        + Duration::days((seed % 365) as i64);
    let start = anchor - Duration::days(day_count.saturating_sub(1) as i64);
    let state = AppState::new(db.clone());
    let mut summary = FixtureSummary {
        wallets: wallet_count,
        strategies: 0,
        positions: 0,
        rolls: 0,
        closed_positions: 0,
        alerts: 0,
        ticks: 0,
    };
    let mut position_counter = 0usize;
    let mut rng_ticks = StdRng::seed_from_u64(seed ^ 0x7469_636b_7321);

    for day in 0..day_count {
        let day_at = start + Duration::days(day as i64);
        crate::prices::tick_once_with_rng(&state, &mut rng_ticks);
        let prices = state.spot_prices.lock().unwrap().clone();
        let mut tick_prices: Vec<_> = prices.iter().collect();
        tick_prices.sort_by_key(|(underlying, _)| *underlying);
        for (tick_index, (underlying, price)) in tick_prices.into_iter().enumerate() {
            let tick_id = format!("fixture-tick-{day:05}-{tick_index:02}");
            sqlx::query(
                "INSERT INTO price_ticks (id, underlying, price, observed_at)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(tick_id)
            .bind(underlying)
            .bind(price)
            .bind(day_at)
            .execute(&db)
            .await?;
            summary.ticks += 1;
        }

        for (wallet_index, wallet) in wallets.iter().enumerate() {
            let symbols = ["XLM", "BTC", "ETH", "SOL"];
            let underlying = symbols[rng.gen_range(0..symbols.len())];
            let spot = prices[underlying];
            let strategy_id = format!("fixture-strategy-{wallet_index:04}-{day:05}");
            let lower_strike = spot * rng.gen_range(0.94..=0.99);
            let upper_strike = spot * rng.gen_range(1.01..=1.06);
            let legs = vec![
                request_for_leg(&mut rng, underlying, lower_strike, "put"),
                request_for_leg(&mut rng, underlying, upper_strike, "call"),
            ];
            let opened = api_result(
                strategies::execute_strategy(
                    State(state.clone()),
                    AuthUser(wallet.clone()),
                    AppJson(ExecuteStrategyRequest { legs }),
                )
                .await,
            )?
            .0;
            for (leg_index, position) in opened.iter().enumerate() {
                let position_id = format!("fixture-position-{position_counter:08}-{leg_index}");
                canonicalize_position_id(&db, &position.id, &position_id).await?;
                sqlx::query("UPDATE positions SET strategy_id = ? WHERE id = ?")
                    .bind(&strategy_id)
                    .bind(&position_id)
                    .execute(&db)
                    .await?;
                summary.positions += 1;
            }
            stamp_strategy(&db, &strategy_id, day_at).await?;
            position_counter += 1;
            summary.strategies += 1;

            if day >= 2 || day_count == 1 {
                let close_day_index = if day_count == 1 { day } else { day - 2 };
                let close_target = if day_count == 1 {
                    strategy_id.clone()
                } else {
                    format!("fixture-strategy-{wallet_index:04}-{close_day_index:05}")
                };
                let closed = api_result(
                    strategies::close_strategy(
                        State(state.clone()),
                        AuthUser(wallet.clone()),
                        AxumPath(close_target.clone()),
                    )
                    .await,
                )?
                .0;
                summary.closed_positions += closed.len();
                stamp_strategy(
                    &db,
                    &close_target,
                    start + Duration::days(close_day_index as i64),
                )
                .await?;
            }

            if day == 0 {
                let request = request_for_leg(&mut rng, underlying, spot, "call");
                let opened = api_result(
                    positions::open_position(
                        State(state.clone()),
                        AuthUser(wallet.clone()),
                        AppJson(request),
                    )
                    .await,
                )?
                .0;
                let old_id = format!("fixture-roll-{wallet_index:04}-old");
                canonicalize_position_id(&db, &opened.id, &old_id).await?;
                summary.positions += 1;
                let rolled = api_result(
                    positions::roll_position(
                        State(state.clone()),
                        AuthUser(wallet.clone()),
                        AxumPath(old_id.clone()),
                        AppJson(RollPositionRequest {
                            new_strike: spot * 1.04,
                            new_expiry_days: 60.0,
                        }),
                    )
                    .await,
                )?
                .0;
                let new_id = format!("fixture-roll-{wallet_index:04}-new");
                canonicalize_position_id(&db, &rolled.opened.id, &new_id).await?;
                let rolled_closed_at = day_at + Duration::hours(2);
                stamp_position(&db, &old_id, day_at, Some(rolled_closed_at)).await?;
                stamp_position(&db, &new_id, rolled_closed_at, None).await?;
                summary.positions += 1;
                summary.rolls += 1;
                summary.closed_positions += 1;

                if wallet_index % 2 == 0 {
                    let closed = api_result(
                        positions::close_position(
                            State(state.clone()),
                            AuthUser(wallet.clone()),
                            AxumPath(new_id.clone()),
                        )
                        .await,
                    )?
                    .0;
                    stamp_position(
                        &db,
                        &new_id,
                        closed.opened_at,
                        Some(day_at + Duration::hours(4)),
                    )
                    .await?;
                    summary.closed_positions += 1;
                }

                let add = crate::watchlist::add_watchlist(
                    State(state.clone()),
                    AuthUser(wallet.clone()),
                    AppJson(crate::watchlist::AddWatchlistRequest {
                        underlying: underlying.to_string(),
                    }),
                )
                .await;
                api_result(add)?;
                sqlx::query("UPDATE watchlist SET added_at = ? WHERE wallet_address = ?")
                    .bind(day_at)
                    .bind(wallet)
                    .execute(&db)
                    .await?;

                for (alert_index, target_price) in
                    [spot * 0.95, spot * 1.10].into_iter().enumerate()
                {
                    let alert = api_result(
                        alerts::create_alert(
                            State(state.clone()),
                            AuthUser(wallet.clone()),
                            AppJson(CreateAlertRequest {
                                underlying: underlying.to_string(),
                                condition: "above".to_string(),
                                target_price,
                            }),
                        )
                        .await,
                    )?
                    .0;
                    let alert_id = format!("fixture-alert-{wallet_index:04}-{alert_index}");
                    sqlx::query("UPDATE alerts SET id = ?, created_at = ? WHERE id = ?")
                        .bind(alert_id)
                        .bind(day_at)
                        .bind(alert.id)
                        .execute(&db)
                        .await?;
                    summary.alerts += 1;
                }
                alerts::check_once(&state).await;
                sqlx::query(
                    "UPDATE alerts
                        SET triggered_at = CASE WHEN triggered = 1 THEN ? ELSE NULL END
                      WHERE wallet_address = ?",
                )
                .bind(day_at + Duration::hours(1))
                .bind(wallet)
                .execute(&db)
                .await?;
                sqlx::query("UPDATE accounts SET created_at = ? WHERE wallet_address = ?")
                    .bind(day_at)
                    .bind(wallet)
                    .execute(&db)
                    .await?;
            }
        }
    }

    Ok(summary)
}

/// Creates a consistent SQLite snapshot, removes authentication data, and
/// replaces every account wallet reference with a salted pseudonym.
pub async fn anonymize_snapshot(from: &str, to: &str) -> AdminResult<()> {
    let source_path = from
        .strip_prefix("sqlite://")
        .ok_or_else(|| admin_error("snapshot source must use sqlite://"))?;
    let destination_path = to
        .strip_prefix("sqlite://")
        .ok_or_else(|| admin_error("snapshot destination must use sqlite://"))?;
    if Path::new(destination_path).exists() {
        return Err(admin_error("snapshot destination already exists"));
    }
    let source_absolute = std::fs::canonicalize(source_path)?;
    let destination_absolute = if Path::new(destination_path).is_absolute() {
        Path::new(destination_path).to_path_buf()
    } else {
        std::env::current_dir()?.join(destination_path)
    };
    if source_absolute == destination_absolute {
        return Err(admin_error("snapshot source and destination must differ"));
    }
    let mut source_options = SqliteConnectOptions::from_str(from)?.read_only(true);
    source_options = source_options.create_if_missing(false);
    let source = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(source_options)
        .await?;
    let escaped_destination = destination_path.replace('\'', "''");
    sqlx::query(&format!("VACUUM INTO '{escaped_destination}'"))
        .execute(&source)
        .await?;
    source.close().await;

    let mut connection = SqliteConnection::connect(to).await?;
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut connection)
        .await?;
    let mut tx = connection.begin().await?;

    for table in ["sessions", "auth_nonces"] {
        sqlx::query(&format!("DELETE FROM {table}"))
            .execute(&mut *tx)
            .await?;
    }

    let addresses: Vec<String> = sqlx::query("SELECT wallet_address FROM accounts")
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    let mut salt = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut salt);
    for address in addresses {
        let mut hasher = Sha256::new();
        hasher.update(salt);
        hasher.update(address.as_bytes());
        let replacement = format!("anon_{}", hex::encode(hasher.finalize()));
        for table in ["positions", "alerts", "watchlist"] {
            sqlx::query(&format!(
                "UPDATE {table} SET wallet_address = ? WHERE wallet_address = ?"
            ))
            .bind(&replacement)
            .bind(&address)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE accounts SET wallet_address = ? WHERE wallet_address = ?")
            .bind(replacement)
            .bind(address)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut connection)
        .await?;
    let violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut connection)
        .await?;
    if !violations.is_empty() {
        return Err(admin_error(format!(
            "anonymized snapshot contains {} foreign-key violations",
            violations.len()
        )));
    }
    connection.close().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_url(name: &str) -> (String, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("zenith-admin-{name}-{}.db", uuid::Uuid::new_v4()));
        (format!("sqlite://{}", path.display()), path)
    }

    async fn fixture_digest(db: &SqlitePool) -> AdminResult<Vec<String>> {
        let accounts: Vec<Account> =
            sqlx::query_as("SELECT * FROM accounts ORDER BY wallet_address")
                .fetch_all(db)
                .await?;
        let positions: Vec<Position> = sqlx::query_as("SELECT * FROM positions ORDER BY id")
            .fetch_all(db)
            .await?;
        let alerts: Vec<Alert> = sqlx::query_as("SELECT * FROM alerts ORDER BY id")
            .fetch_all(db)
            .await?;
        let watchlist: Vec<WatchlistItem> =
            sqlx::query_as("SELECT * FROM watchlist ORDER BY wallet_address, underlying")
                .fetch_all(db)
                .await?;
        let ticks: Vec<PriceTickRow> = sqlx::query_as("SELECT * FROM price_ticks ORDER BY id")
            .fetch_all(db)
            .await?;
        Ok(vec![
            rows_checksum(&accounts)?,
            rows_checksum(&positions)?,
            rows_checksum(&alerts)?,
            rows_checksum(&watchlist)?,
            rows_checksum(&ticks)?,
        ])
    }

    #[tokio::test]
    async fn fixture_generation_is_repeatable_for_a_seed() {
        let (first_url, first_path) = db_url("fixtures-first");
        let (second_url, second_path) = db_url("fixtures-second");
        let first = crate::db::init_pool(&first_url).await;
        let second = crate::db::init_pool(&second_url).await;

        let first_summary = generate_fixtures(first.clone(), 2, 3, 42).await.unwrap();
        let second_summary = generate_fixtures(second.clone(), 2, 3, 42).await.unwrap();

        assert_eq!(first_summary.wallets, 2);
        assert_eq!(first_summary.strategies, 6);
        assert_eq!(first_summary.rolls, 2);
        assert_eq!(first_summary.alerts, 4);
        assert_eq!(first_summary.ticks, 12);
        assert_eq!(first_summary, second_summary);
        let first_positions: Vec<serde_json::Value> =
            sqlx::query_as::<_, Position>("SELECT * FROM positions ORDER BY id")
                .fetch_all(&first)
                .await
                .unwrap()
                .iter()
                .map(serde_json::to_value)
                .collect::<Result<_, _>>()
                .unwrap();
        let second_positions: Vec<serde_json::Value> =
            sqlx::query_as::<_, Position>("SELECT * FROM positions ORDER BY id")
                .fetch_all(&second)
                .await
                .unwrap()
                .iter()
                .map(serde_json::to_value)
                .collect::<Result<_, _>>()
                .unwrap();
        for (first_position, second_position) in first_positions.iter().zip(&second_positions) {
            assert_eq!(first_position, second_position);
        }
        assert_eq!(
            fixture_digest(&first).await.unwrap(),
            fixture_digest(&second).await.unwrap()
        );

        first.close().await;
        second.close().await;
        let _ = std::fs::remove_file(first_path);
        let _ = std::fs::remove_file(second_path);
    }

    #[tokio::test]
    async fn snapshot_pseudonymizes_wallets_and_strips_auth_rows() {
        let (source_url, source_path) = db_url("snapshot-source");
        let (destination_url, destination_path) = db_url("snapshot-copy");
        let source = crate::db::init_pool(&source_url).await;
        generate_fixtures(source.clone(), 1, 1, 7).await.unwrap();
        let wallet: String = sqlx::query_scalar("SELECT wallet_address FROM accounts")
            .fetch_one(&source)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO sessions (token, wallet_address, expires_at)
             VALUES ('fixture-session', ?, '2026-12-31T00:00:00.000Z')",
        )
        .bind(&wallet)
        .execute(&source)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO auth_nonces (nonce, wallet_address, expires_at)
             VALUES ('fixture-nonce', ?, '2026-12-31T00:00:00.000Z')",
        )
        .bind(&wallet)
        .execute(&source)
        .await
        .unwrap();

        anonymize_snapshot(&source_url, &destination_url)
            .await
            .unwrap();
        let snapshot = SqlitePoolOptions::new()
            .connect(&destination_url)
            .await
            .unwrap();
        let pseudonym: String = sqlx::query_scalar("SELECT wallet_address FROM accounts")
            .fetch_one(&snapshot)
            .await
            .unwrap();
        assert!(pseudonym.starts_with("anon_"));
        assert_ne!(pseudonym, wallet);
        let auth_rows: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM sessions) + (SELECT COUNT(*) FROM auth_nonces)",
        )
        .fetch_one(&snapshot)
        .await
        .unwrap();
        assert_eq!(auth_rows, 0);
        let foreign_key_violations = sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&snapshot)
            .await
            .unwrap();
        assert!(foreign_key_violations.is_empty());

        snapshot.close().await;
        source.close().await;
        let _ = std::fs::remove_file(source_path);
        let _ = std::fs::remove_file(destination_path);
    }
}
