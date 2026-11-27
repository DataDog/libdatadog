#!/usr/bin/env bash
# Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

exec "$RC_NATIVE_ZIG" cc "$@" -target "$RC_NATIVE_ZIG_TARGET"
