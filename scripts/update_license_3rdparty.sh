#!/usr/bin/env sh

# Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -eu

# If you're missing dd-rust-license-tool, install it with:
# cargo install dd-rust-license-tool

cd "$(dirname "$0")"/..
dd-rust-license-tool write
