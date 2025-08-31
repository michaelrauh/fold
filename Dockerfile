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
    cp target/x86_64-unknown-linux-musl/release/queue_checker ./queue_checker && \
    cp target/x86_64-unknown-linux-musl/release/db_checker ./db_checker && \
    cp target/x86_64-unknown-linux-musl/release/interner_util ./interner_util && \
    cp target/x86_64-unknown-linux-musl/release/s3_util ./s3_util

FROM alpine
WORKDIR /app
COPY --from=builder /home/rust/src/fold_worker /app/fold_worker
COPY --from=builder /home/rust/src/follower /app/follower  
COPY --from=builder /home/rust/src/feeder /app/feeder
COPY --from=builder /home/rust/src/queue_checker /app/queue_checker
COPY --from=builder /home/rust/src/db_checker /app/db_checker
COPY --from=builder /home/rust/src/interner_util /app/interner_util
COPY --from=builder /home/rust/src/s3_util /app/s3_util
COPY wait-for-it.sh /app/wait-for-it.sh

# Ensure executables have the right mode
RUN chmod +x /app/fold_worker /app/follower /app/feeder /app/queue_checker /app/db_checker /app/interner_util /app/s3_util /app/wait-for-it.sh

USER 1000
