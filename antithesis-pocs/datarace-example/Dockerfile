FROM rust:1.91-slim AS builder

WORKDIR /app
COPY . .

RUN cargo build

FROM debian:bookworm-slim

WORKDIR /app
COPY --from=builder /app/target/debug/datarace-example .

CMD [ "./datarace-example" ]
