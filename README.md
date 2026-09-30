# Zenith Backend

Rust/Axum API for Zenith, a decentralized options protocol on Stellar
Soroban. Black-Scholes pricing with a crypto vol smile, a paper-trading
account/positions ledger backed by SQLite, sign-in-with-wallet auth, and
a live spot-price WebSocket feed.

## Status

Market data (spot prices, vol surface) is in-memory and nudged by a
background simulator — there's no real price feed or on-chain
integration yet. Everything else (accounts, positions, watchlist,
alerts) persists to a SQLite file via sqlx. This is a paper-trading
backend for the frontend to build against, not a production trading
system.

## Getting started

```bash
cp .env.example .env   # DATABASE_URL=sqlite://zenith.db, or leave unset for the same default
cargo run
# listening on 0.0.0.0:8081
```

```bash
cargo test              # unit + integration tests, plus the property-based invariant harness
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

No external services required — sqlx creates and migrates the SQLite
file on first run, and every integration test spins up its own
throwaway temp-file database.

em.

## Getting started

```bash
cp .env.example .env   # DATABASE_URL=sqlite://zenith.db, or leave unset for the same default
cargo run
# listening on 0.0.0.0:8081
```

```bash
cargo test              # unit + integration tests, plus the property-based invariant harness
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

No external services required — sqlx creates and migrates the SQLite
file on first run, and every integration test spins up its own
throwaway temp-file database.

Secrets are resolved through a pluggable `SecretProvider` (env vars by
default; SOPS files and HashiCorp Vault are also supported) and cached in a
`SecretStore` that fails fast at startup and fails soft on refresh. HMAC keys
rotate through a sign-with-current / verify-with-current-or-previous ring, and
Stellar signing can be delegated to Vault Transit or AWS KMS so the private
key never enters process memory. See `docs/secrets-rotation.md`.

`tests/invariants_test.rs` is a property-based state-machine harness: it
generates random sequences of ledger operations, steps a pure in-memory
reference model and the real router in lockstep, and re-checks the
global invariants (conservation, no negative balances, locked collateral
equals the open legs' collateral, valid status transitions, cross-wallet
isolation) after every step. It runs with a bounded case count in CI;
the nightly workflow raises that via `PROPTEST_CASES`. Failing inputs
shrink to a minimal sequence and are persisted under
`proptest-regressions/`.

The fund-critical math (collateral, payoff, fees) is additionally
model-checked with [Kani](https://model-checking.github.io/kani/) over the
integer-scaled prototypes in `math_fixed.rs` — see `docs/verification.md`
for the property list, the `cargo kani` command, and the documented `f64`
gap. `.github/workflows/kani.yml` runs it on every PR and nightly.


## Endpoints

All `/api/v1/*` endpoints marked **auth** require an
`Authorization: Bearer <token>` header from `/api/v1/auth/verify`.

### Market data (public)

| Endpoint | What it does |
|---|---|
| `GET /health` | Liveness + a DB ping |
| `GET /api/v1/spot` | Current spot prices + base vols for all underlyings |
| `GET /api/v1/price` | Black-Scholes premium/Greeks for one option |
| `GET /api/v1/iv` | Implied vol for a given market price (Newton-Raphson) |
| `GET /api/v1/chain` | Full optio

## Endpoints

All `/api/v1/*` endpoints marked **auth** require an
`Authorization: Bearer <token>` header from `/api/v1/auth/verify`.

### Market data (public)

| Endpoint | What it does |
|---|---|
| `GET /health` | Liveness + a DB ping |
| `GET /api/v1/spot` | Current spot prices + base vols for all underlyings |
| `GET /api/v1/price` | Black-Scholes premium/Greeks for one option |
| `GET /api/v1/iv` | Implied vol for a given market price (Newton-Raphson) |
| `GET /api/v1/chain` | Full option chain (calls+puts) across strikes for one expiry |
| `GET /api/v1/expiries/:underlying` | Available expiries for an underlying |
| `GET /api/v1/stats` | Protocol-wide stats (mocked, not derived from real trades) |
| `GET /api/v1/ws/spot` | WebSocket: snapshot on connect, then a live tick every ~2s |
| `POST /api/v1/portfolio/payoff` | Combined P&L curve for a set of caller-supplied legs (no auth — legs carry their own premium) |

### Auth (public, rate-limited)

| Endpoint | What it does |
|---|---|
| `POST /api/v1/auth/nonce` | Issue a single-use, 5-minute sign-in message for a wallet address |
| `POST /api/v1/auth/verify` | Verify the signed message, get a 24h bearer session token |
| `GET /api/v1/auth/me` **auth** | Confirm the current token's wallet address |

Every `POST` below (positions open/close/roll, watchlist/alerts create,
strategies/execute) is rate-limited per-IP (5/s, burst 20) on top of
requiring a session — see `mutation_rate_limited_routes()` in `lib.rs`.

### Account & positions **auth**

| Endpoint | What it does |
|---|---|
| `GET /api/v1/account` | Balance + locked collateral |
| `GET /api/v1/positions` | List positions (`?status=`, `?strategy_id=`, `?limit=`, `?offset=`) |
| `POST /api/v1/positions/open` | Price and open one position |
| `POST /api/v1/positions/:id/close` | Settle an open position at current spot/vol |
| `POST /api/v1/positions/:id/roll` | Close + reopen at a new strike/expiry, atomically |
| `GET /api/v1/history` | Closed/rolled positions + win/loss/pnl stats (`?limit=`, `?offset=` — stats always cover the full history, not just the returned page) |
| `GET /api/v1/portfolio/greeks` | Aggregate Greeks across all open positions, repriced live |
| `POST /api/v1/strategies/execute` | Open 2+ legs atomically under one shared `strategy_id` |
| `GET /api/v1/strategies` | List multi-leg strategies with aggregate realized/unrealized P&L |
| `GET /api/v1/strategies/:id` | One strategy's legs + aggregate P&L |
| `POST /api/v1/strategies/:id/close` | Close every currently-open leg of a strategy atomically |

### Watchlist & alerts **auth**

| Endpoint | What it does |
|---|---|
| `GET` / `POST /api/v1/watchlist` | List / add a watched symbol |
| `DELETE /api/v1/watchlist/:underlying` | Remove a watched symbol |
| `GET` / `POST /api/v1/alerts` | List / create a price alert (`above`/`below` a target) |
| `DELETE /api/v1/alerts/:id` | Remove an alert |

Alerts are checked against spot every 10s by a background task; a
triggered alert stays in the table (visible via GET) rather than being
deleted.

Every response carries an `x-request-id` header — a fresh UUIDv4 if the
request didn't already have one, or the caller's own value echoed back
unchanged otherwise — for tracing a single request through logs.

## Architecture

```
src/
├── main.rs          # Thin entrypoint: init_tracing -> init_state -> build_router -> serve
├── lib.rs           # Pricing engine, AppState, request/response types, route wiring
├── db.rs            # SQLite pool + migration runner
├── models.rs        # Row structs (Account, Position, WatchlistItem, Alert)
├── error.rs         # AppError: JSON {"error": "..."} instead of empty-body status codes
├── auth.rs          # Sign-in-with-wallet: nonce, verify, AuthUser extractor, session cleanup
├── strkey.rs         # Stellar G... address <-> raw ed25519 pubkey codec
├── collateral.rs    # Collateral rules for writing options (100% calls, 110% puts)
├── payoff.rs         # Combined multi-leg P&L math (ported from the frontend's lib/payoff.ts)
├── secrets/           # SecretProvider (env/SOPS/Vault), SecretStore, HMAC key rings, SigV4
├── signing.rs         # Signer trait + local / Vault Transit / AWS KMS ed25519 signers
├── math_fixed.rs     # Integer-scaled prototypes of collateral/payoff/fee math (Kani-verified)
├── kani_proofs.rs    # Kani harnesses for math_fixed, compiled only under `cargo kani`
├── positions.rs      # Account/position/roll/greeks handlers + the open/close tx helpers
├── strategies.rs     # Multi-leg atomic execution, built on positions.rs's tx helpers
├── history.rs         # Closed/rolled positions + stats
├── request_id.rs      # UUIDv4 generator for the x-request-id middleware
├── watchlist.rs, alerts.rs, prices.rs  # Per-domain CRUD + background loops
migrations/           # One file per schema change, embedded into the binary at compile time
tests/
├── common/mod.rs     # TestApp: real router over a throwaway temp-file DB, via tower::oneshot
└── *_test.rs         # One file per domain, black-box HTTP-level assertions
```

`main.rs` is intentionally thin — everything testable lives in the
`zenith_backend` library crate, which is what lets `tests/*_test.rs`
exercise the real router without a bin-only crate's usual restriction
(a `tests/` directory can only see a *library* crate's public items).

### A note on the pricing model

`smile_vol()` is a bit-for-bit port of the frontend's `smileVol()` —
including its wing term being unconditionally `(|moneyness-1|-0.15)^2`
rather than clamped at zero near the money, which looks like a bug but
matches the frontend's shipped (if quirky) behavior on purpose, for
pricing parity between client and server.

### A note on time-to-expiry

Closing/rolling a position reprices it with the *same* time-to-expiry
it was opened with, rather than tracking an absolute expiry timestamp
and computing real elapsed time. Fine for a paper-trading demo; not a
real theta-decay model.

## Known gaps

- No real market data feed — spot prices are seeded constants nudged by
  a random-walk simulator, not sourced from anywhere real.
- No on-chain / Soroban integration — this is pure off-chain paper
  trading.
- `Dockerfile` and the CI workflow are not build/run-tested against a
  real Docker daemon or GitHub Actions runner from this environment —
  reviewed for correctness, not executed end-to-end.
- The mutation rate limiter's bearer-token fallback (for requests with
  no token at all) keys on peer IP only, without replicating
  SmartIpKeyExtractor's x-forwarded-for/x-real-ip/forwarded header
  chain — acceptable since that fallback path is only reached by
  requests that fail AuthUser's own check regardless, but it does mean
  that one specific path isn't proxy-aware the way the auth endpoints'
  limiter is.

Previously listed here and since addressed: the three background loops
(auth cleanup, alert checks, price simulator) now have direct unit
tests against their extracted per-tick logic; the rate limiter moved
off a single global quota to per-IP (`SmartIpKeyExtractor`) on the auth
endpoints and per-wallet (`BearerOrIpKeyExtractor`) on every mutating
one (positions, watchlist, alerts, strategies), so wallets sharing an
IP no longer share a quota; `list_positions`/`get_history` now report
total count and whether more pages exist (`x-total-count`/`x-has-more`
headers on positions, a `has_more` field on history) instead of
leaving a paging client to guess; malformed query params and JSON
bodies now return the same `{"error": "..."}` shape as every other
failure instead of axum's plain-text rejections (`AppQuery`/`AppJson`
in `error.rs`); every remaining unexpected-DB-failure path across the
whole app now names the specific operation that failed (`db_error()`
in `error.rs`) instead of a bare "Internal Server Error" with no other
detail; and cross-wallet ownership on close/roll (a stranger 404s
trying to close or roll another wallet's position, same as it already
did for deleting someone else's alert/watchlist entry) and
roll_position's replacement-leg validation are now actually tested
rather than just assumed from reading the SQL.

## License

MIT © Zenith Protocol Contributors
