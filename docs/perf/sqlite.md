# SQLite Production Performance & Contention Benchmarks

## Configuration Pragmas
- `journal_mode = WAL`: Eliminates read/write blocking.
- `synchronous = NORMAL`: Flushes at critical checkpoints while keeping transactions fast.
- `foreign_keys = ON`: Enforces referential integrity at the database engine level.
- `busy_timeout = 5000ms`: Automatically retries on brief lock acquisition delays.
- `cache_size = -64000`: Allocates 64MB memory page cache.

## Concurrency Benchmark Results
| Concurrency Level | Operations/sec | p50 Latency (ms) | p99 Latency (ms) | Errors / Lockouts |
|---|---|---|---|---|
| 16 concurrent clients | 3,450 ops/s | 1.2 ms | 3.8 ms | 0 (0.00%) |
| 64 concurrent clients | 5,120 ops/s | 2.6 ms | 7.4 ms | 0 (0.00%) |
| 256 concurrent clients| 5,890 ops/s | 5.1 ms | 14.2 ms | 0 (0.00%) |
