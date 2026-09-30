use zenith_backend::migration_lint::MigrationLinter;

#[test]
fn test_all_migrations_pass_lint() {
    let migration_files = [
        include_str!("../migrations/0007_network_metadata.sql"),
        include_str!("../migrations/0008_sponsorship_budgets.sql"),
        include_str!("../migrations/0009_contract_wasm_history.sql"),
        include_str!("../migrations/0010_leases_and_keeper.sql"),
    ];

    for file in migration_files {
        let result = MigrationLinter::lint_sql(file);
        assert!(result.is_ok(), "Migration failed safety lint: {:?}", result);
    }
}
