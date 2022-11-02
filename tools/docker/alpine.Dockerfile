ARG ALPINE_BASE_IMAGE="alpine:3.16"
ARG CARGO_BUILD_INCREMENTAL="true"
ARG CARGO_NET_RETRY="2"
ARG CBINGEN_VERSION="0.24.3"

FROM ${ALPINE_BASE_IMAGE} as base
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

FROM base as aws_cli
    RUN apk add --no-cache \
        python3 \
        py3-pip \
        groff \
    && pip3 install --upgrade pip \
    && pip3 install --no-cache-dir \
        awscli \
    && rm -rf /var/cache/apk/*

    RUN aws --version   # Just to make sure its installed alright

# // TODO: export registry cache as published docker image, and reference it here
FROM scratch AS cargo_registry_cache

FROM base as cbindgen
    ENV PATH="/root/.cargo/bin:$PATH"
    ARG CARGO_BUILD_INCREMENTAL
    ARG CARGO_NET_RETRY
    ARG CBINGEN_VERSION
    ENV CARGO_NET_RETRY="${CARGO_NET_RETRY}"
    ARG TARGETPLATFORM
    ARG BUILDPLATFORM
    COPY --from=cargo_registry_cache / /root/.cargo/registry/
    RUN set -xe; \
            cargo install cbindgen --version ${CBINGEN_VERSION}; \
            rm -rf /root/.cargo/registry /root/.cargo/git

FROM aws_cli as builder
    COPY --from=cargo_registry_cache / /root/.cargo/registry/
    COPY --from=cbindgen /root/.cargo/bin/cbindgen /usr/local/bin/cbindgen
