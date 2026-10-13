# Note: This image is intended to be used for local development and testing and not for building release
# artifacts or CI runners.
FROM ubuntu:latest

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    gnupg \
    lsb-release \
    build-essential \
    cmake \
    protobuf-compiler \
    docker.io \
    sudo \
    wget \
    && rm -rf /var/lib/apt/lists/*

# We need go in order to build aws-lc-fips-sys
RUN wget -O go1.24.2.linux-arm64.tar.gz https://go.dev/dl/go1.24.2.linux-arm64.tar.gz \
    && tar -C /usr/local -xzf go1.24.2.linux-arm64.tar.gz

# Docker-in-Docker configuration (necessary for integration tests)
RUN mkdir -p /var/lib/docker
EXPOSE 2375

# allow non-root to write to the dockerd logs
RUN mkdir -p /var/log/dockerd && \
    chmod 777 /var/log/dockerd

# Shell script that starts dockerd and switches to user. We need to start as root for docker-in-docker then
# switch to a non-root user for tests.
RUN echo '#!/usr/bin/env bash\n\
dockerd --host=unix:///var/run/docker.sock --host=tcp://0.0.0.0:2375 > /var/log/dockerd/dockerd.log 2>&1 &\n\
exec su - user\n' \
    > /usr/local/bin/start-dockerd.sh

RUN chmod +x /usr/local/bin/start-dockerd.sh

#create and use a non-root user. This is necessary for certain tests that expect the user to not be root.
RUN useradd -m -u 1001 -g 1000 -s /bin/bash user
RUN usermod -aG docker root
RUN usermod -aG docker user

WORKDIR /home/user

# Copy toolchain files so the Rust install reads pinned versions from source of truth
COPY rust-toolchain.toml nightly-toolchain.toml /home/user/

# Install Rust toolchain for user in the usual ~/.cargo location.
# Stable and nightly versions are read from the toolchain files
RUN su - user -c "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y"
RUN su - user -c "cargo install --locked 'cargo-nextest@0.9.96'"
RUN NIGHTLY=$(awk -F'"' '/^channel[[:space:]]*=/ {print $2}' /home/user/nightly-toolchain.toml) && \
    su - user -c "rustup install $NIGHTLY"
RUN STABLE=$(awk -F'"' '/^channel[[:space:]]*=/ {print $2}' /home/user/rust-toolchain.toml) && \
    su - user -c "rustup default $STABLE"

# Use a different target to not interfere with the host which may be a different arch
RUN echo 'export CARGO_TARGET_DIR=/libdatadog/docker-linux-target' >> /home/user/.bashrc

CMD ["/usr/local/bin/start-dockerd.sh"]
