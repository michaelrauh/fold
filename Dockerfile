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
RUN apt-get update && apt-get install -y libssl3 curl postgresql-client bash && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/fold_worker .
COPY --from=builder /app/target/release/follower .
COPY --from=builder /app/target/release/feeder .
COPY --from=builder /app/target/release/ingestor .
COPY wait-for-it.sh .
RUN chmod +x ./wait-for-it.sh
