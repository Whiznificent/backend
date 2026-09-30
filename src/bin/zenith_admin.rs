//! `zenith-admin` — operator CLI for the Zenith indexer.
//!
//! Currently exposes the historical replay tool required by issue #46:
//!
//! ```text
//! zenith-admin indexer replay --from-ledger <N> --to-ledger <M> [--projections-only]
//! ```
//!
//! Replay rebuilds *projections* (derived data) from the immutable raw event
//! store. Projections are idempotent and may be truncated and rebuilt, so a
//! replay is always safe to re-run. A lock row is taken for the duration of the
//! replay so that live ingestion cannot race the rebuild.
//!
//! See `docs/runbooks/indexer-replay.md` for the operator runbook.

use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use zenith::indexer::projections::{ProjectionStore, PROJECTION_SCHEMA_VERSION};

/// Advisory-lock key guarding a replay against concurrent live ingestion.
const REPLAY_LOCK_KEY: i64 = 0x7a65_6e69_7468_0001;

#[derive(Parser, Debug)]
#[command(name = "zenith-admin", version, about = "Zenith operator CLI")]
struct Cli {
    /// Postgres connection string. Falls back to `DATABASE_URL`.
    #[arg(long, env = "DATABASE_URL", global = true)]
    database_url: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Indexer maintenance commands.
    Indexer(IndexerArgs),
}

#[derive(Args, Debug)]
struct IndexerArgs {
    #[command(subcommand)]
    command: IndexerCommand,
}

#[derive(Subcommand, Debug)]
enum IndexerCommand {
    /// Rebuild projections from the raw event store for a ledger range.
    Replay(ReplayArgs),
    /// Verify that processed ledger ranges are contiguous.
    CheckGaps,
}

#[derive(Args, Debug)]
struct ReplayArgs {
    /// First ledger (inclusive) to replay.
    #[arg(long)]
    from_ledger: u32,

    /// Last ledger (inclusive) to replay.
    #[arg(long)]
    to_ledger: u32,

    /// Only rebuild projections; do not touch raw events or checkpoints.
    #[arg(long, default_value_t = false)]
    projections_only: bool,

    /// Number of ledgers to process per batch (memory bound).
    #[arg(long, default_value_t = 1_000)]
    batch_size: u32,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<()> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&cli.database_url)
        .await
        .context("connecting to Postgres")?;

    match cli.command {
        Command::Indexer(args) => match args.command {
            IndexerCommand::Replay(replay) => replay_projections(&pool, replay).await,
            IndexerCommand::CheckGaps => check_gaps(&pool).await,
        },
    }
}

/// Rebuild projections for `[from_ledger, to_ledger]` idempotently.
///
/// A session-level advisory lock serialises replays against live ingestion.
/// Raw events are never mutated; only derived projections are truncated and
/// rebuilt, in bounded batches so memory stays flat for large ranges.
async fn replay_projections(pool: &PgPool, args: ReplayArgs) -> Result<()> {
    if args.from_ledger > args.to_ledger {
        bail!(
            "--from-ledger ({}) must be <= --to-ledger ({})",
            args.from_ledger,
            args.to_ledger
        );
    }
    if args.batch_size == 0 {
        bail!("--batch-size must be greater than zero");
    }

    let mut conn = pool.acquire().await.context("acquiring connection")?;

    // Serialise against live ingestion. `pg_try_advisory_lock` fails fast so an
    // operator learns immediately that a replay is already running.
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(REPLAY_LOCK_KEY)
        .fetch_one(&mut *conn)
        .await
        .context("acquiring replay advisory lock")?;
    if !locked {
        bail!("another replay is already in progress (advisory lock held)");
    }

    let result = replay_locked(&mut conn, &args).await;

    // Always release the lock, even on failure.
    let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(REPLAY_LOCK_KEY)
        .execute(&mut *conn)
        .await;

    result
}

async fn replay_locked(conn: &mut sqlx::PgConnection, args: &ReplayArgs) -> Result<()> {
    let store = ProjectionStore::new();

    info!(
        from_ledger = args.from_ledger,
        to_ledger = args.to_ledger,
        projections_only = args.projections_only,
        schema_version = PROJECTION_SCHEMA_VERSION,
        "starting projection replay"
    );

    // Truncate the derived projections for the affected range. Raw events are
    // immutable and are left untouched.
    store
        .truncate_range(conn, args.from_ledger, args.to_ledger)
        .await
        .context("truncating projections for replay range")?;

    let mut cursor = args.from_ledger;
    let mut replayed: u64 = 0;
    while cursor <= args.to_ledger {
        let batch_end = cursor
            .saturating_add(args.batch_size.saturating_sub(1))
            .min(args.to_ledger);

        let rows = sqlx::query(
            "SELECT ledger_sequence, event_index, payload \
             FROM indexer_raw_events \
             WHERE ledger_sequence BETWEEN $1 AND $2 \
             ORDER BY ledger_sequence, event_index",
        )
        .bind(cursor as i64)
        .bind(batch_end as i64)
        .fetch_all(&mut *conn)
        .await
        .context("loading raw events for replay batch")?;

        for row in &rows {
            let ledger: i64 = row.try_get("ledger_sequence")?;
            let event_index: i32 = row.try_get("event_index")?;
            let payload: Vec<u8> = row.try_get("payload")?;
            store
                .apply_raw_event(conn, ledger as u32, event_index, &payload)
                .await
                .with_context(|| {
                    format!("applying raw event ledger={ledger} index={event_index}")
                })?;
            replayed += 1;
        }

        info!(batch_end, replayed, "replay batch complete");
        cursor = batch_end.saturating_add(1);
    }

    // Record the replay in the checkpoint table so operators can audit it.
    if !args.projections_only {
        sqlx::query(
            "INSERT INTO indexer_checkpoints (from_ledger, to_ledger, kind, recorded_at) \
             VALUES ($1, $2, 'replay', now())",
        )
        .bind(args.from_ledger as i64)
        .bind(args.to_ledger as i64)
        .execute(&mut *conn)
        .await
        .context("recording replay checkpoint")?;
    }

    info!(replayed, "projection replay complete");
    Ok(())
}

/// Detect non-contiguous processed ledger ranges and raise an alert.
///
/// Returns a non-zero exit code when a gap is found so it can be wired into
/// monitoring / cron alerting.
#[allow(clippy::cast_possible_wrap)]
async fn check_gaps(pool: &PgPool) -> Result<()> {
    let rows = sqlx::query(
        "SELECT from_ledger, to_ledger \
         FROM indexer_checkpoints \
         WHERE kind = 'processed' \
         ORDER BY from_ledger",
    )
    .fetch_all(pool)
    .await
    .context("loading processed checkpoints")?;

    let mut expected: Option<i64> = None;
    let mut gaps: Vec<(i64, i64)> = Vec::new();

    for row in &rows {
        let from: i64 = row.try_get("from_ledger")?;
        let to: i64 = row.try_get("to_ledger")?;
        if let Some(prev_end) = expected {
            if from > prev_end + 1 {
                gaps.push((prev_end + 1, from - 1));
            }
        }
        expected = Some(match expected {
            Some(prev_end) => prev_end.max(to),
            None => to,
        });
    }

    if gaps.is_empty() {
        info!(ranges = rows.len(), "no ledger gaps detected");
        return Ok(());
    }

    for (start, end) in &gaps {
        warn!(gap_start = start, gap_end = end, "non-contiguous ledger range detected");
    }
    bail!("detected {} ledger gap(s); replay required", gaps.len());
}
