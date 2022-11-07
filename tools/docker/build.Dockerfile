# syntax=docker/dockerfile:1.4

FROM ghcr.io/datadog/libdatadog-ci:debian_builder_stable as base
   
FROM base as improve_caching
    RUN --mount=target=/build bash tools/docker/src_caching.sh

FROM scratch as src_cargo
    COPY --from=improve_caching /output/cargo /

# // TODO: export registry cache as published docker image, and reference it here
FROM scratch AS cargo_registry_cache

### Image for building using rust nightly
FROM ghcr.io/datadog/libdatadog-ci:debian_builder_nightly as debian_nightly_builder

### FFI builder
FROM base AS ffi_build
  WORKDIR /build
  COPY --from=cargo_registry_cache / /root/.cargo/registry/
  COPY --from=src_cargo / .

  # cache debug dependency build
  RUN cargo build --lib --all
  # cache release dependency build
  RUN cargo build --release --lib --all

  COPY ./ ./
  RUN ./build-profiling-ffi.sh /build/output/profiling

FROM scratch as build_ffi
  COPY --from=ffi_build /build/output/ ./

