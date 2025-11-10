#!/usr/bin/env sh

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -eu

# If you're missing cargo bundle-licenses, install it with:
# cargo install cargo-bundle-licenses

cd "$(dirname "$0")"/..
CARGO_HOME=/tmp/dd-cargo cargo bundle-licenses --format yaml --output LICENSE-3rdparty.yml
