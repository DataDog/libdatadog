// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::ffi::CStr;

/// The discoverable name of the memory mapping.
pub const MAPPING_NAME: &CStr = c"OTEL_CTX";
