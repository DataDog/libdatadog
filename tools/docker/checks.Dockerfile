FROM ghcr.io/datadog/libdatadog-ci-base:debian-stable as base

FROM base as improve_caching
    RUN --mount=target=/build bash tools/docker/src_caching.sh

FROM scratch as src_rs
    COPY --from=improve_caching /output/rs_src /

FROM scratch as src_cargo
    COPY --from=improve_caching /output/cargo /

FROM scratch as src_other
    COPY --from=improve_caching /output/other_src /

FROM base as check_base 
    # cache cargo registry
    RUN cargo search nothing --limit 1
    
    # cache dependencies
    COPY --from=src_cargo / .
    # cache debug dependency build
    RUN cargo build --lib --all
    # cache release dependency build
    RUN cargo build --release --lib --all

FROM base as check_license_headers
    COPY --from=src_rs / /build/
    COPY --from=src_other / /build/
    RUN ! find . -type f | xargs licensecheck -c '.*' | grep -v 'Apache License 2.0'

FROM check_base as build_license_3rdparty_file
    COPY --from=src_cargo / /build/
    RUN mkdir /output
    RUN cargo bundle-licenses \
            --format yaml --output /output/LICENSE-3rdparty.yml

FROM scratch as export_license_3rdparty_file
    COPY --from=build_license_3rdparty_file /output/LICENSE-3rdparty.yml /

FROM check_base as check_license_3rdparty_file
    COPY --from=src_cargo / .
    COPY LICENSE-3rdparty.yml .
    RUN find /build
    RUN cargo bundle-licenses \
            --format yaml \
            --output /tmp/CI.yaml \
            --previous LICENSE-3rdparty.yml \
            --check-previous

FROM check_base as check_rust_fmt
    COPY --from=src_cargo / .
    COPY --from=src_rs / .
    COPY rustfmt.toml .
    RUN set -xe; cargo fmt --all -- --check

FROM check_base as check_clippy
    COPY --from=src_cargo / .
    COPY --from=src_rs / .
    RUN set -xe; cargo clippy --all-targets --all-features -- -D warnings