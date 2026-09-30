use zenith_backend::db::{detect_backend, DatabaseBackend};

#[test]
fn test_database_scheme_detection() {
    assert_eq!(
        detect_backend("postgres://user:pass@localhost:5432/zenith"),
        DatabaseBackend::Postgres
    );
    assert_eq!(
        detect_backend("postgresql://user:pass@localhost:5432/zenith"),
        DatabaseBackend::Postgres
    );
    assert_eq!(
        detect_backend("sqlite://zenith.db"),
        DatabaseBackend::Sqlite
    );
    assert_eq!(
        detect_backend("sqlite::memory:"),
        DatabaseBackend::Sqlite
    );
}
