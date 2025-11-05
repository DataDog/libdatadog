FROM rust:1.91-slim AS builder

WORKDIR /app
COPY . .

RUN cargo build --release

FROM debian:bookworm-slim

WORKDIR /app
COPY --from=builder /app/target/release/datarace-example .

CMD ["./datarace-example"]

