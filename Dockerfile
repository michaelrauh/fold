# syntax=docker/dockerfile:1
FROM ekidd/rust-musl-builder:stable AS builder

COPY . .
RUN --mount=type=cache,target=/home/rust/.cargo/git \
    --mount=type=cache,target=/home/rust/.cargo/registry \
    --mount=type=cache,sharing=private,target=/home/rust/src/target \
    sudo chown -R rust: target /home/rust/.cargo && \
    rm -f Cargo.lock && \
    cargo build --release && \
    cp target/x86_64-unknown-linux-musl/release/fold_worker ./fold_worker && \
    cp target/x86_64-unknown-linux-musl/release/follower ./follower && \
    cp target/x86_64-unknown-linux-musl/release/feeder ./feeder && \
    cp target/x86_64-unknown-linux-musl/release/feed_util ./feed_util

FROM alpine
RUN apk --no-cache add curl postgresql-client && \
    wget -O /usr/local/bin/mc https://dl.min.io/client/mc/release/linux-amd64/mc && \
    chmod +x /usr/local/bin/mc
WORKDIR /app
COPY --from=builder /home/rust/src/fold_worker /app/fold_worker
COPY --from=builder /home/rust/src/follower /app/follower  
COPY --from=builder /home/rust/src/feeder /app/feeder
COPY --from=builder /home/rust/src/feed_util /app/feed_util
COPY wait-for-it.sh /app/wait-for-it.sh
COPY scripts/ /app/scripts/

# Ensure executables have the right mode
RUN chmod +x /app/fold_worker /app/follower /app/feeder /app/feed_util /app/wait-for-it.sh /app/scripts/*.sh

USER 1000
