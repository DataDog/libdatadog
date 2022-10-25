ARG ALPINE_BASE_IMAGE="alpine:3.16"
ARG CARGO_BUILD_INCREMENTAL="true"
ARG CARGO_NET_RETRY="2"
ARG BUILDER_IMAGE=debian_builder
ARG DEBIAN_RUST_IMAGE=rust:1-slim-bullseye
ARG DEBIAN_RUST_NIGHTLY_IMAGE=rustlang/rust:nightly-bullseye-slim

### Debian builder
FROM ${DEBIAN_RUST_IMAGE} as debian_builder
  ENV CARGO_HOME="/root/.cargo"
  WORKDIR /build
  RUN cargo install cbindgen; mv /root/.cargo/bin/cbindgen /usr/bin/; rm -rf /root/.cargo

### Debian buildplatform builder
FROM --platform=$BUILDPLATFORM ${DEBIAN_RUST_IMAGE} as debian_builder_platform_native
  ENV CARGO_HOME="/root/.cargo"
  WORKDIR /build

### Alpine builder
FROM ${ALPINE_BASE_IMAGE} as alpine_base
  ENV CARGO_HOME="/root/.cargo"
  WORKDIR /build

  RUN apk update \
    && apk add --no-cache \
      build-base \
      cargo \
      curl \
      git \
      make \
      patchelf \
      protoc \
      pkgconf \
      unzip \
      bash \
    && mkdir /usr/local/src

  # Tell docker to use bash as the default
  SHELL ["/bin/bash", "-c"]

  # Don't use rustup! For some reason it provides different native-static-libs
  # and this can cause problems for users.
  # Also, it doesn't understand x86_64-alpine-linux-musl like the OS's cargo.
  #RUN rustup-init -y --no-modify-path --default-toolchain stable

FROM alpine_base as alpine_aws_cli
  RUN apk add --no-cache \
    python3 \
    py3-pip \
    groff \
    && pip3 install --upgrade pip \
    && pip3 install --no-cache-dir \
    awscli \
    && rm -rf /var/cache/apk/*

  RUN aws --version   # Just to make sure its installed alright

FROM alpine_base as alpine_cbindgen
  ENV PATH="/root/.cargo/bin:$PATH"
  ARG CARGO_BUILD_INCREMENTAL
  ARG CARGO_NET_RETRY
  ENV CARGO_NET_RETRY="${CARGO_NET_RETRY}"
  RUN cargo install cbindgen && rm -rf /root/.cargo/registry /root/.cargo/git

FROM alpine_aws_cli as alpine_builder
  COPY --from=alpine_cbindgen /root/.cargo/bin/cbindgen /usr/local/bin/cbindgen

### Image for building using rust nightly
FROM ${DEBIAN_RUST_NIGHTLY_IMAGE} as debian_nightly_builder
  ENV CARGO_HOME="/root/.cargo"
  WORKDIR /build
  RUN cargo install cbindgen; mv /root/.cargo/bin/cbindgen /usr/bin/; rm -rf /root/.cargo

### Cache cargo metadata between builds
FROM debian_builder_platform_native AS ffi_build_platform_agnostic_cache_build
  # update cache cargo.io metadata
  RUN cargo search nothing --limit 1 

  # create stubs to cache compilation of dependendencies
  COPY [ "Cargo.lock", "Cargo.toml", "./"]
  COPY "ddcommon/Cargo.toml" "ddcommon/" 
  COPY "ddcommon-ffi/Cargo.toml" "ddcommon-ffi/" 
  COPY "ddtelemetry/Cargo.toml" "ddtelemetry/"
  COPY "ddtelemetry-ffi/Cargo.toml" "ddtelemetry-ffi/"
  COPY "profiling/Cargo.toml" "profiling/"
  COPY "profiling-ffi/Cargo.toml" "profiling-ffi/"
  COPY "tools/Cargo.toml" "tools/" 
  RUN find -name "Cargo.toml" | sed -e s#Cargo.toml#src/lib.rs#g | xargs -n 1 sh -c 'mkdir -p $(dirname $1); touch $1; echo $1' create_stubs
  RUN echo tools/src/bin/dedup_headers.rs ddtelemetry-ffi/src/bin/ddtelemetry-ffi-header.rs ddtelemetry/examples/tm-worker-test.rs | xargs -n 1 sh -c 'mkdir -p $(dirname $1); touch $1; echo $1' create_stubs

  # cache dependencies
  RUN cargo fetch --locked

# extract cargo cache
FROM --platform=$BUILDPLATFORM scratch as ffi_build_platform_agnostic_cache
  COPY --from=ffi_build_platform_agnostic_cache_build /root/.cargo /root/.cargo
  COPY --from=ffi_build_platform_agnostic_cache_build /build /build

### FFI builder
FROM ${BUILDER_IMAGE} AS ffi_build
  COPY --from=ffi_build_platform_agnostic_cache /root/.cargo /root/.cargo/
  COPY --from=ffi_build_platform_agnostic_cache /build /build
  WORKDIR /build
  # cache debug dependency build
  RUN cargo build --lib --all
  # cache release dependency build
  RUN cargo build --release --lib --all

  COPY ./ ./
  RUN --mount=type=cache,target=/build/target ./build-profiling-ffi.sh /build/output/profiling
  RUN --mount=type=cache,target=/build/target ./build-telemetry-ffi.sh /build/output/telemetry

### cbindgen @nightly builder
FROM debian_nightly_builder AS cbindgen_build
  COPY --from=ffi_build_platform_agnostic_cache /root/.cargo /root/.cargo/
  COPY --from=ffi_build_platform_agnostic_cache /build /build
  WORKDIR /build
  # cache debug dependency build
  RUN cargo build --lib --all
  # cache release dependency build
  RUN cargo build --release --lib --all 

  COPY ./ ./
  RUN --mount=id=cbindgen,type=cache,target=/build/target ./tools/scripts/generate_headers.sh /build/output/profiling
  RUN --mount=id=cbindgen,type=cache,target=/build/target ./tools/scripts/generate_headers.sh /build/output/telemetry

FROM scratch as ffi_build_output
  COPY --from=ffi_build /build/output/ ./
  # overwrite headers generated with stable build with nightly-built headers
  COPY --from=cbindgen_build /build/output/ ./
