# Secret rotation runbook

Every secret the backend uses is fetched through a `SecretProvider`
(`src/secrets/mod.rs`) and refreshed on a timer by `SecretStore`. This runbook
is the operating procedure for each class of secret. It is written so a
rotation never requires a redeploy and never invalidates live client state.

## How the plumbing behaves

- **Startup is fail-fast.** `SecretStore::load` fetches the whole configured
  set up front and returns an error if any secret is missing or the backend is
  unreachable, so a half-configured process never accepts traffic.
- **Runtime is fail-soft.** `SecretStore::refresh_loop` re-fetches on an
  interval. If the backend is briefly unavailable it logs and keeps serving the
  last known values rather than collapsing mid-flight.
- **Values are `secrecy::SecretString`.** They are redacted in `Debug`,
  zeroised on drop, and only exposed (`ExposeSecret`) at the point of use. Never
  log a secret, and never `println!("{value:?}")` a config struct that holds one.
- **Backends** are selected with `ZENITH_SECRET_BACKEND`:
  - `env` (default) — one environment variable per secret.
  - `sops` — a SOPS file (`SOPS_FILE`), decrypted on demand by the `sops`
    binary; plaintext never touches disk.
  - `vault` — Vault KV v2 (`VAULT_ADDR`, `VAULT_TOKEN`, `VAULT_SECRET_MOUNT`),
    with `auth/token/renew-self` lease renewal.

## Secret classes and their procedures

### 1. HMAC keys (cursors, idempotency, webhook signatures)

These are dealt with by a ring (`HmacKeyRing`): sign with the **current** key,
verify with current **or previous**. Rotation is therefore a three-step
operation with no invalid-signature window.

1. Generate a new key.
2. Set `<NAME>` to the new key and `<NAME>_PREVIOUS` to the old one.
3. Wait out the longest TTL of anything signed with the old key (cursor
   pages, idempotency records, retried webhooks), then remove
   `<NAME>_PREVIOUS`.

`HmacKeyRing::verify` accepts either key; `has_previous()` tells you whether a
rotation window is still open, and this is the signal to keep the old key
configured. Removing `_PREVIOUS` completes the rotation and immediately stops
honouring the old key.

### 2. Database credentials

1. Create the new credential in the database.
2. Update the secret in the backend (or, for Vault, let the credential lease
   expire the old one after the overlap window).
3. Trigger a refresh (wait for the interval, or restart once).
4. Confirm the app is connected with the new credential, then revoke the old.

### 3. Stellar signing keys (sponsor, oracle, SEP-10)

With `VaultTransitSigner` or `AwsKmsSigner`, the private key **never enters this
process** — only a key name/ID and a service credential do. Rotating one is a
matter of publishing the new public key on-chain/at the service, pointing the
`Signer` at the new key, and then deleting the old key once no in-flight
transaction references it. Because signing is remote, there is no in-memory key
to scrub and no restart that could briefy hold two keys.

`LocalEd25519Signer` is **development only**; it holds a seed in process memory
and must not be used for the sponsor/oracle keys.

### 4. Vault token

`VaultSecretProvider` renews its own token lease (`renew_lease`) on every store
refresh. A new token is a config change plus one refresh; because the cache is
only swapped when *all* secrets resolve, a bad token leaves the old values in
place and raises an alert rather than breaking the process.

## Verification after a rotation

- The store's next refresh succeeds (no `secret refresh failed` logs).
- For HMAC rings, a token/record signed before the rotation still verifies
  while `_PREVIOUS` is set.
- For signing keys, `Signer::public_key()` returns the new key and a small
  sign/verify round-trip succeeds.

## Testing

The provider and signer paths are unit-tested against a scripted `HttpClient`
so every response shape (200/404/5xx/malformed JSON) is exercised without a
live dependency. For a full Vault integration test, run Vault locally (for
example with testcontainers) and point `VAULT_ADDR`/`VAULT_TOKEN` at it.
