# syntax=docker/dockerfile:1
FROM rustlang/rust:nightly-bookworm AS builder
WORKDIR /app
# 1. Cache dependencies
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release || true
# 2. Copy actual source and build all binaries
RUN rm src/main.rs
COPY src ./src
COPY benches ./benches
RUN cargo build --release

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y libssl3 curl postgresql-client bash \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app

# Copy the release artifacts with cleaner paths
COPY --from=builder /app/target/release/fold_worker /app/fold_worker
COPY --from=builder /app/target/release/follower /app/follower
COPY --from=builder /app/target/release/feeder /app/feeder
COPY --from=builder /app/target/release/ingestor /app/ingestor
COPY wait-for-it.sh /app/wait-for-it.sh

# Ensure executables have the right mode
RUN chmod +x /app/fold_worker /app/follower /app/feeder /app/ingestor /app/wait-for-it.sh
