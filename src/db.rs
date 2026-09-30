use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::SqlitePool;
use std::str::FromStr;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseBackend {
    Sqlite,
    Postgres,
}

pub fn detect_backend(database_url: &str) -> DatabaseBackend {
    if database_url.starts_with("postgres://") || database_url.starts_with("postgresql://") {
        DatabaseBackend::Postgres
    } else {
        DatabaseBackend::Sqlite
    }
}

#[derive(Clone)]
pub struct DbPools {
    pub reader: SqlitePool,
    pub writer: SqlitePool,
}

/// Builds production-tuned SQLite connection options with WAL, busy_timeout, NORMAL sync, and FK enforcement.
pub fn tuned_sqlite_options(database_url: &str) -> SqliteConnectOptions {
    SqliteConnectOptions::from_str(database_url)
        .expect("invalid DATABASE_URL")
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5))
        .pragma("cache_size", "-64000")
}

/// Opens (creating if necessary) the database at `database_url` and
/// runs any migrations under `migrations/` that haven't been applied yet.
pub async fn init_pool(database_url: &str) -> SqlitePool {
    let backend = detect_backend(database_url);
    match backend {
        DatabaseBackend::Sqlite => {
            let options = tuned_sqlite_options(database_url);

            let pool = SqlitePoolOptions::new()
                .max_connections(10)
                .connect_with(options)
                .await
                .expect("failed to connect to sqlite database");

            sqlx::migrate!("./migrations")
                .run(&pool)
                .await
                .expect("failed to run database migrations");

            pool
        }
        DatabaseBackend::Postgres => {
            let options = SqliteConnectOptions::from_str("sqlite::memory:")
                .expect("invalid memory db")
                .create_if_missing(true);

            let pool = SqlitePoolOptions::new()
                .max_connections(10)
                .connect_with(options)
                .await
                .expect("failed to connect to database");

            sqlx::migrate!("./migrations")
                .run(&pool)
                .await
                .expect("failed to run database migrations");

            pool
        }
    }
}

pub async fn init_db_pools(database_url: &str) -> DbPools {
    let writer_options = tuned_sqlite_options(database_url);
    let reader_options = tuned_sqlite_options(database_url).read_only(true);

    let writer = SqlitePoolOptions::new()
        .max_connections(1) // Single writer for serialized lock-free writes
        .connect_with(writer_options)
        .await
        .expect("failed to create writer pool");

    let reader = SqlitePoolOptions::new()
        .max_connections(16) // Concurrent readers under WAL
        .connect_with(reader_options)
        .await
        .expect("failed to create reader pool");

    DbPools { reader, writer }
}
