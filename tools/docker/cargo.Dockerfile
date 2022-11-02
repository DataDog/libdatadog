# syntax=docker/dockerfile:1.4

ARG RUST_BASE_IMAGE=rust:1-slim-bullseye
# Build only used to generate cargo registry cache
# as ARM builds use extreme amounts of memory under qemu (10GB +) emulation
# when first processing the registry
FROM --platform=$BUILDPLATFORM ${RUST_BASE_IMAGE} as base
    RUN mkdir /build
    ENV CARGO_HOME="/root/.cargo"
    WORKDIR /build
    # install Rust build dependencies that do not require compilation or cargo registry operations
    # to improve cross platform builds
    RUN set -xe; rustup component add rustfmt clippy

FROM --platform=$BUILDPLATFORM base as generate_cargo_cache
    RUN cargo search nothing --limit 1

FROM scratch as output
    COPY --from=generate_cargo_cache /root/.cargo/registry/ /
