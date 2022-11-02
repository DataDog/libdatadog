ARG RUST_BASE_IMAGE=rust:1-slim-bullseye
ARG CARGO_BUILD_INCREMENTAL="true"
ARG CARGO_NET_RETRY="2"
ARG BUNDLE_LICENSES_VERSION=1.0.1
ARG CBINGEN_VERSION="0.24.3"

FROM ghcr.io/datadog/libdatadog-ci:cargo_registry_cache AS cargo_registry_cache

FROM ${RUST_BASE_IMAGE} as builder
    RUN mkdir /build
    ENV CARGO_HOME="/root/.cargo"
    WORKDIR /build
    # install Rust build dependencies that do not require compilation or cargo registry operations
    # to improve cross platform builds
    RUN set -xe; rustup component add rustfmt clippy

    COPY --from=cargo_registry_cache / /root/.cargo/registry/

    RUN apt-get update && apt-get install -y \
        curl \
        licensecheck 
    ARG BUNDLE_LICENSES_VERSION
    ARG CARGO_BUILD_INCREMENTAL
    ARG CARGO_NET_RETRY
    ARG CBINGEN_VERSION
    ARG TARGETPLATFORM
    ARG BUILDPLATFORM
    RUN set -xe; \
        cargo install cargo-bundle-licenses --version ${BUNDLE_LICENSES_VERSION}; \
        cargo install cbindgen --version ${CBINGEN_VERSION}; \
        mv /root/.cargo/bin/cbindgen /usr/local/bin/cbindgen; \
        rm -rf /root/.cargo/git; 
