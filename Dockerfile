FROM rust:1-slim AS chef
WORKDIR /app
# The query macros resolve their metadata from the committed .sqlx/ directory
# at compile time; SQLX_OFFLINE=true keeps `cargo build` from trying to open a
# database connection during the build (there is none in this stage).
ENV SQLX_OFFLINE=true

# sqlx's "sqlite" feature builds SQLite from source via libsqlite3-sys,
# which needs a C compiler — the slim image doesn't ship one by default.
RUN apt-get update && apt-get install -y --no-install-recommends build-essential \
    && rm -rf /var/lib/apt/lists/* \
    && cargo install cargo-chef --locked

# Cache dependency compilation separately from the app source: this
# layer only invalidates when Cargo.toml/Cargo.lock change, not on
# every source edit.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && echo "fn main() {}" > src/worker.rs && echo "" > src/lib.rs
RUN cargo build --release && rm -rf src

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked \
    && mkdir -p /image-data \
    && touch /image-data/.keep

# --- runtime stage -------------------------------------------------------
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

FROM gcr.io/distroless/cc-debian12:nonroot
WORKDIR /app
# migrations/ isn't needed here — sqlx::migrate!() embeds the SQL files
# into the binary at compile time, not read from disk at runtime.
COPY --from=builder --chown=65532:65532 /app/target/release/zenith-backend ./
COPY --from=builder --chown=65532:65532 /app/target/release/zenith-worker ./
COPY --from=builder --chown=65532:65532 /image-data /data

ENV DATABASE_URL=sqlite:///data/zenith.db
VOLUME ["/data"]
EXPOSE 8081
USER 65532:65532
ENTRYPOINT ["/app/zenith-backend"]
