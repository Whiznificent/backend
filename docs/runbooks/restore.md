# Restore runbook

How to restore the Zenith database from the Litestream replica, and how to
verify the restore is good.

## Backup architecture

The backend writes its SQLite database to `/data/zenith.db` (see the
`Dockerfile`). A [Litestream](https://litestream.io/) sidecar watches that
file and continuously replicates it to object storage.

- **Configuration:** `deploy/litestream.yml`
- **Docker Compose example:** `deploy/docker-compose.yml`
- **Object storage:** any S3-compatible bucket (AWS S3, MinIO, R2, ...).
  Credentials come from the `LITESTREAM_*` environment variables.

### RPO (recovery point objective): under 10 seconds

Litestream does not take periodic snapshots — it tails the database's WAL
(write-ahead log) and uploads each frame to the replica as it is written.
There is no polling interval to tune for a smaller RPO; the replica is
continuously streaming. In practice the replica lags the primary by less
than a second during normal operation and at most a few seconds under load,
so the RPO is **well under 10 seconds**. A transaction committed on the
primary is replicated as part of the next WAL frame, not on a timer.

## Restore procedure

### 1. Pick a target timestamp

The restore is point-in-time: you choose the moment to restore to. Find a
timestamp from your monitoring, incident timeline, or the object storage's
versioning. Use RFC 3339 / ISO 8601 UTC, e.g. `2026-09-28T12:34:56Z`.

### 2. Run the restore script

```bash
scripts/restore.sh <target-timestamp> [output-path]
```

- `<target-timestamp>` — the moment to restore to (required).
- `[output-path]` — where to write the restored database
  (default `/tmp/zenith-restore.db`).

The script uses the `litestream` binary if it is on `PATH`, otherwise the
`litestream/litestream:v0.5.17` container. It reads the config from
`LITESTREAM_CONFIG` (default `deploy/litestream.yml`) and the credentials
from the `LITESTREAM_*` environment variables.

### 3. What the script checks

The script refuses to report success unless all three checks pass:

1. **`PRAGMA integrity_check`** returns `ok` — the database is not
   corrupted at the page level.
2. **Migration version check** — the restored database has no failed
   migrations and its `MAX(version)` in `_sqlx_migrations` matches the
   latest migration in `migrations/`. A restore that stopped halfway
   through a migration would fail here.
3. **Ledger reconciliation invariant** — for every account,
   `balance = 100000 + Σ(open cash flows) + Σ(close cash flows)`. This
   catches a restore that is missing or duplicating positions, which an
   integrity check alone would not.

### 4. Promote the restored database

Once the checks pass, the restored file is a ordinary SQLite database. To
bring it back into service, stop the backend, replace `/data/zenith.db`
(and its `-wal`/`-shm` if present) with the restored file, and start the
backend again. Verify with `GET /health` before declaring the incident
over.

## RTO (recovery time objective)

The RTO is measured by the nightly restore drill (see below): the time from
starting the restore to all three checks passing. The script prints it as
`RTO: <N>ms`.

| Drill date | RTO | Notes |
|---|---|---|
| _paste from the latest CI run here_ | | |

## Nightly restore drill

`.github/workflows/backup-drill.yml` runs every night at 03:00 UTC (and on
demand). It:

1. Starts a MinIO test bucket.
2. Seeds a throwaway database with the schema and one settled position.
3. Replicates it to MinIO with Litestream.
4. Runs `scripts/restore.sh` against the replica and asserts all three
   checks pass.

A backup that has never been restored is a hope, not a backup — the drill
is what makes the RPO/RTO numbers above real. When a drill fails, treat it
as an incident: the restore path is broken until it passes again.

## Upgrading Litestream

The version is pinned in three places — bump them together:

- `deploy/litestream.yml` is config-only (no version), but the image tag in
  `deploy/docker-compose.yml`
- the `litestream/litestream:v0.5.17` fallback image in `scripts/restore.sh`
- the `litestream-v0.5.17-...` download URL in
  `.github/workflows/backup-drill.yml`
