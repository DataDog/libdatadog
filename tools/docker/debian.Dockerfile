ARG RUST_BASE_IMAGE=rust:1-slim-bullseye
ARG CARGO_BUILD_INCREMENTAL="true"
ARG CARGO_NET_RETRY="2"
ARG BUNDLE_LICENSES_VERSION=1.0.1
ARG CBINGEN_VERSION="0.24.3"
FROM ${RUST_BASE_IMAGE} as builder
    RUN mkdir /build
    ENV CARGO_HOME="/root/.cargo"
    WORKDIR /build
    RUN --mount=type=cache,target=/var/lib/apt/lists/ apt-get update && apt-get install -y \
        curl \
        licensecheck 
    # install Rust build dependencies
    RUN set -xe; rustup component add rustfmt clippy
    ARG BUNDLE_LICENSES_VERSION
    ARG CARGO_BUILD_INCREMENTAL
    ARG CARGO_NET_RETRY
    ARG CBINGEN_VERSION
    RUN set -xe; \
        cargo install cargo-bundle-licenses --version ${BUNDLE_LICENSES_VERSION}; \
        cargo install cbindgen --version ${CBINGEN_VERSION}; \
        rm -rf /root/.cargo/registry /root/.cargo/git; 

