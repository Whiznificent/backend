#!/usr/bin/env bash
#
# Point-in-time restore of the Zenith database from a Litestream replica,
# followed by validation. See docs/runbooks/restore.md for the full
# runbook.
#
# Usage:
#   scripts/restore.sh <target-timestamp> [output-path]
#
#   <target-timestamp>  RFC 3339 timestamp to restore to, e.g.
#                       2026-09-28T12:34:56Z (or "now" for the latest)
#   [output-path]       where to write the restored database
#                       (default: /tmp/zenith-restore.db)
#
# Environment:
#   LITESTREAM_CONFIG   path to the litestream config
#                       (default: deploy/litestream.yml)
#   LITESTREAM_*        credentials matching the config (bucket, endpoint,
#                       region, access-key-id, secret-access-key)
#
# Requires: litestream (or docker) and sqlite3.

set -euo pipefail

TIMESTAMP="${1:?usage: restore.sh <target-timestamp> [output-path]}"
OUT="${2:-/tmp/zenith-restore.db}"
CONFIG="${LITESTREAM_CONFIG:-deploy/litestream.yml}"

if [ ! -f "$CONFIG" ]; then
  echo "ERROR: litestream config not found at $CONFIG" >&2
  exit 1
fi

START_NS=$(date +%s%N)

# ── 1. Restore ──────────────────────────────────────────────────────────────
# Prefer the litestream binary; fall back to the official container.
if command -v litestream >/dev/null 2>&1; then
  litestream restore -config "$CONFIG" -timestamp "$TIMESTAMP" -o "$OUT"
else
  docker run --rm \
    -e LITESTREAM_BUCKET="${LITESTREAM_BUCKET:-}" \
    -e LITESTREAM_ENDPOINT="${LITESTREAM_ENDPOINT:-}" \
    -e LITESTREAM_REGION="${LITESTREAM_REGION:-}" \
    -e LITESTREAM_ACCESS_KEY_ID="${LITESTREAM_ACCESS_KEY_ID:-}" \
    -e LITESTREAM_SECRET_ACCESS_KEY="${LITESTREAM_SECRET_ACCESS_KEY:-}" \
    -v "$(pwd)/$CONFIG:/litestream.yml:ro" \
    -v "$(dirname "$OUT"):/out" \
    litestream/litestream:v0.5.17 restore \
      -config /litestream.yml -timestamp "$TIMESTAMP" -o "/out/$(basename "$OUT")"
fi

if [ ! -f "$OUT" ]; then
  echo "ERROR: restore did not produce $OUT" >&2
  exit 1
fi
echo "restored database to $OUT"

# ── 2. PRAGMA integrity_check ───────────────────────────────────────────────
INTEGRITY="$(sqlite3 "$OUT" "PRAGMA integrity_check;")"
if [ "$INTEGRITY" != "ok" ]; then
  echo "FAIL: integrity_check returned: $INTEGRITY" >&2
  exit 1
fi
echo "ok: integrity_check"

# ── 3. Migration version check ──────────────────────────────────────────────
# The restored database must be at a real, fully-applied migration state:
# no failed migrations, and a max version matching the latest migration in
# the repo (the drill restores the latest backup, so they must agree).
# sqlx stores the migration version as the numeric prefix of the filename
# (0001_accounts.sql -> version 1), so compare as numbers.
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXPECTED="$(ls "$REPO_ROOT"/migrations/*.sql 2>/dev/null \
  | sed 's/.*\///;s/_.*//' | sort -n | tail -1)"
LATEST="$(sqlite3 "$OUT" "SELECT MAX(version) FROM _sqlx_migrations;")"
FAILED="$(sqlite3 "$OUT" "SELECT COUNT(*) FROM _sqlx_migrations WHERE success = 0;")"
if [ "$FAILED" != "0" ]; then
  echo "FAIL: $FAILED migration(s) failed in the restored database" >&2
  exit 1
fi
if [ "$LATEST" != "$EXPECTED" ]; then
  echo "FAIL: restored migration version $LATEST != expected $EXPECTED" >&2
  exit 1
fi
echo "ok: migration version $LATEST"

# ── 4. Ledger reconciliation invariant ───────────────────────────────────────
# Every account's balance must equal its starting balance (100000) plus the
# sum of all cash flows from its positions: premium paid/received on open,
# and settlement received/paid on close. Any row returned here is a
# violation.
VIOLATIONS="$(sqlite3 "$OUT" "
SELECT a.wallet_address, a.balance AS actual, e.expected
  FROM accounts a
  JOIN (
        SELECT wallet_address,
               100000.0 + COALESCE(SUM(
                   (CASE WHEN position_type = 'short' THEN 1.0 ELSE -1.0 END)
                       * entry_premium * contracts
                   + CASE WHEN status IN ('closed', 'rolled')
                          THEN (CASE WHEN position_type = 'short' THEN -1.0 ELSE 1.0 END)
                               * close_premium * contracts
                          ELSE 0.0 END
               ), 0.0) AS expected
          FROM positions
         GROUP BY wallet_address
       ) e ON e.wallet_address = a.wallet_address
 WHERE ABS(a.balance - e.expected) > 0.01;")"
if [ -n "$VIOLATIONS" ]; then
  echo "FAIL: ledger reconciliation invariant violated:" >&2
  echo "$VIOLATIONS" >&2
  exit 1
fi
echo "ok: ledger reconciliation invariant"

# ── RTO ─────────────────────────────────────────────────────────────────────
END_NS=$(date +%s%N)
RTO_MS=$(( (END_NS - START_NS) / 1000000 ))
echo "RTO: ${RTO_MS}ms (restore + validation)"
echo "PASS: restore drill succeeded"
