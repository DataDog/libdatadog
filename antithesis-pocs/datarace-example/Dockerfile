FROM rust:1.91-slim AS builder

WORKDIR /app
COPY . .

COPY ./ressources/libvoidstar.so /usr/lib/libvoidstar.so

RUN LD_LIBRARY_PATH=/usr/lib/ RUSTFLAGS="                                      \
    -Ccodegen-units=1                                                          \
    -Cpasses=sancov-module                                                     \
    -Cllvm-args=-sanitizer-coverage-level=3                                    \
    -Cllvm-args=-sanitizer-coverage-trace-pc-guard                             \
    -Clink-args=-Wl,--build-id                                                 \
    -L/usr/lib/                                                                \
    -lvoidstar"                                                                \
    cargo build --release

FROM debian:bookworm-slim

WORKDIR /app
COPY --from=builder /app/target/release/datarace-example .
COPY --from=builder /usr/lib/libvoidstar.so /usr/lib/libvoidstar.so

COPY ./ressources/driver.sh /opt/antithesis/test/v1/main/serial_driver_datarace-v0
