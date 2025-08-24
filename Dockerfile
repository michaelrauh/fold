# syntax=docker/dockerfile:1

# Make the Dockerfile platform-aware so multi-arch builds via
# `docker buildx build --platform=linux/amd64 ...` produce the
# correct linux/amd64 binaries for clusters while keeping local
# docker-compose runs working when building for the host platform.
ARG BUILDPLATFORM
ARG TARGETPLATFORM
ARG TARGETARCH
FROM --platform=$TARGETPLATFORM rustlang/rust:nightly-bookworm AS builder
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

FROM --platform=$TARGETPLATFORM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y libssl3 curl postgresql-client bash \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app

# Copy the release artifacts. When building with buildx for a specific
# --platform the builder stage will produce binaries for that platform.
COPY --from=builder /app/target/release/fold_worker /app/fold_worker
COPY --from=builder /app/target/release/follower /app/follower
COPY --from=builder /app/target/release/feeder /app/feeder
COPY --from=builder /app/target/release/ingestor /app/ingestor
COPY wait-for-it.sh /app/wait-for-it.sh

# Ensure executables have the right mode
RUN chmod +x /app/fold_worker /app/follower /app/feeder /app/ingestor /app/wait-for-it.sh
