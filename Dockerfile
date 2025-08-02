# syntax=docker/dockerfile:1
FROM rustlang/rust:nightly AS builder
WORKDIR /app
# Copy manifests, benches, then source, then build
COPY Cargo.toml Cargo.lock ./
COPY benches/ benches/
COPY src/ src/
RUN cargo build --release

FROM alpine:latest
WORKDIR /app
RUN apk update && apk add libssl3 curl postgresql-client && rm -rf /var/cache/apk/*
COPY --from=builder /app/target/release/fold /app/fold
COPY wait-for-it.sh /app/wait-for-it.sh
RUN chmod +x /app/wait-for-it.sh /app/fold
CMD ["/app/wait-for-it.sh", "rabbitmq:5672", "--", "/app/fold"]
