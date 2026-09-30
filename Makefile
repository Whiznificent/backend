# Makefile for the Zenith backend.
#
# `duckdb-demo` queries the Parquet exports produced by
# `zenith-admin export run` (see src/export.rs). It needs DuckDB installed
# (https://duckdb.org/docs/installation/) and at least one export run
# against a database that has data.

# The export directory. Must match EXPORT_DIR used for the export run
# (default ./exports).
EXPORT_DIR ?= exports

.PHONY: duckdb-demo

## Query the analytics exports with DuckDB: row counts per dataset, a
## per-underlying settlement summary, and the top wallets by realized P&L
## (addresses are hashed, so this is PII-safe by default).
duckdb-demo:
	duckdb -c " \
		SELECT 'positions' AS dataset, COUNT(*) AS rows FROM read_parquet('$(EXPORT_DIR)/positions/*/*.parquet', union_by_name=true) \
		UNION ALL \
		SELECT 'ledger_entries', COUNT(*) FROM read_parquet('$(EXPORT_DIR)/ledger_entries/*/*.parquet', union_by_name=true) \
		UNION ALL \
		SELECT 'price_candles', COUNT(*) FROM read_parquet('$(EXPORT_DIR)/price_candles/*/*.parquet', union_by_name=true) \
		UNION ALL \
		SELECT 'settlements', COUNT(*) FROM read_parquet('$(EXPORT_DIR)/settlements/*/*.parquet', union_by_name=true);"
	duckdb -c " \
		SELECT underlying, COUNT(*) AS settlements, ROUND(SUM(realized_pnl), 2) AS total_realized_pnl \
		FROM read_parquet('$(EXPORT_DIR)/settlements/*/*.parquet', union_by_name=true) \
		GROUP BY underlying ORDER BY total_realized_pnl DESC;"
	duckdb -c " \
		SELECT wallet_address, COUNT(*) AS trades, ROUND(SUM(realized_pnl), 2) AS total_realized_pnl \
		FROM read_parquet('$(EXPORT_DIR)/settlements/*/*.parquet', union_by_name=true) \
		GROUP BY wallet_address ORDER BY trades DESC LIMIT 10;"
