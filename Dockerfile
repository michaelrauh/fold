# syntax=docker/dockerfile:1
FROM rustlang/rust:nightly as builder
WORKDIR /app
COPY . .
RUN cargo build --release
RUN ls -l /app/target/release/
# Copy all numbered .txt files into the image
COPY 0.txt 1.txt 2.txt 3.txt 4.txt 5.txt 6.txt 7.txt 8.txt 9.txt 10.txt 11.txt 12.txt 13.txt 14.txt 15.txt 16.txt 17.txt 18.txt 19.txt 20.txt 21.txt 22.txt 23.txt 24.txt 25.txt 26.txt 27.txt 28.txt /app/

FROM debian:bookworm-slim
WORKDIR /app
COPY --from=builder /app/target/release/fold /app/fold
COPY --from=builder /app/0.txt /app/0.txt
COPY --from=builder /app/1.txt /app/1.txt
COPY --from=builder /app/2.txt /app/2.txt
COPY --from=builder /app/3.txt /app/3.txt
COPY --from=builder /app/4.txt /app/4.txt
COPY --from=builder /app/5.txt /app/5.txt
COPY --from=builder /app/6.txt /app/6.txt
COPY --from=builder /app/7.txt /app/7.txt
COPY --from=builder /app/8.txt /app/8.txt
COPY --from=builder /app/9.txt /app/9.txt
COPY --from=builder /app/10.txt /app/10.txt
COPY --from=builder /app/11.txt /app/11.txt
COPY --from=builder /app/12.txt /app/12.txt
COPY --from=builder /app/13.txt /app/13.txt
COPY --from=builder /app/14.txt /app/14.txt
COPY --from=builder /app/15.txt /app/15.txt
COPY --from=builder /app/16.txt /app/16.txt
COPY --from=builder /app/17.txt /app/17.txt
COPY --from=builder /app/18.txt /app/18.txt
COPY --from=builder /app/19.txt /app/19.txt
COPY --from=builder /app/20.txt /app/20.txt
COPY --from=builder /app/21.txt /app/21.txt
COPY --from=builder /app/22.txt /app/22.txt
COPY --from=builder /app/23.txt /app/23.txt
COPY --from=builder /app/24.txt /app/24.txt
COPY --from=builder /app/25.txt /app/25.txt
COPY --from=builder /app/26.txt /app/26.txt
COPY --from=builder /app/27.txt /app/27.txt
COPY --from=builder /app/28.txt /app/28.txt
RUN chmod +x /app/fold
CMD ["/app/fold"]
