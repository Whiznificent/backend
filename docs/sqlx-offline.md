# Compile-time checked SQL (sqlx offline mode)

Every production query in this backend goes through sqlx's compile-time
checked macros (`sqlx::query!`, `sqlx::query_as!`, `sqlx::query_scalar!`)
instead of the runtime-checked `sqlx::query()` / `sqlx::query_as()` /
`sqlx::query_scalar()` functions. A typo in a column name, a wrong bind
count, or a type mismatch is now a compile error instead of a runtime
failure.

The macros need the output of SQLite's `DESCRIBE` for each statement.
That metadata is committed under `.sqlx/` as one `query-<hash>.json` file
per query, where `<hash>` is the SHA-256 of the query string. With the
metadata present, the macros compile without a live database
(`SQLX_OFFLINE=true` or simply no `DATABASE_URL` set).

## Regenerating the metadata

After changing any query that uses a macro (or adding a new one), regenerate
the metadata and commit the result:

```bash
# point at a database that has had all migrations applied
DATABASE_URL=sqlite://zenith.db sqlx migrate run   # first time only
DATABASE_URL=sqlite://zenith.db cargo sqlx prepare
```

`cargo sqlx prepare` re-describes every macro query against the live
database and rewrites `.sqlx/`. Commit the changed `query-*.json` files
alongside the code change.

CI runs `cargo sqlx prepare --check` against a freshly migrated database on
every push/PR. It fails if `.sqlx/` is missing a query or if any metadata
file differs from what the live database describes — i.e. exactly when you
forgot to run `cargo sqlx prepare`.

## Building without a database

The Docker build and `cargo build`/`cargo test` have no database available.
The macros fall back to the committed `.sqlx/` metadata automatically when
`DATABASE_URL` is unset. The Dockerfile also sets `SQLX_OFFLINE=true` in
the build stage so a stray `DATABASE_URL` can't trigger a live describe
against a non-existent database.

## Column overrides

SQLite's planner cannot always prove a column's type or nullability (for
expressions, aggregates, and `TEXT PRIMARY KEY` columns, which SQLite
reports as nullable). Where the inferred type would be wrong, the query
uses a column override in the SQL itself:

```sql
SELECT
    id AS "id!",                       -- "!": force non-null
    triggered AS "triggered: bool",    -- ": bool": force this Rust type
    SUM(x) AS "total: _"               -- ": _": infer from the struct field
FROM ...
```

- `name!` — force the column to be treated as non-null.
- `name: Type` — force the column's Rust type.
- `name: _` — infer the column's Rust type from the corresponding field of
  the record struct (used for aggregates whose type the planner can't prove).

The override is part of the column's alias, so it appears in the query
string and therefore in the `.sqlx/` metadata. The macro strips it before
matching the column to a struct field.

## Dynamic queries

`GET /api/v1/positions` (`list_positions`) takes optional `status` and
`strategy_id` filters. Instead of branching into several static queries, it
uses one query whose filters are `(? IS NULL OR status = ?)` — a `NULL`
argument disables that filter. This keeps a single checked statement for all
four filter combinations. The optional-filter behaviour is covered by
`tests/positions_test.rs` (filtering by status, by strategy, and the
unfiltered case).

## Test fixtures

The `#[cfg(test)]` modules and `tests/*_test.rs` build throwaway rows with
the runtime `sqlx::query()` functions on purpose: test setup benefits from
runtime flexibility (varying column lists, hand-built rows), and these
queries are not on any production path. Everything the application actually
runs at request time is behind a checked macro.
