// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::ParallelSet;
use std::ffi::c_void;

/// Represents a link to the active local root span and span. Note that in
/// OpenTelemetry, this uses the trace id instead of the local root span id.
/// We'll cross that bridge when it's time.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Link2 {
    pub local_root_span_id: u64,
    pub span_id: u64,
}

pub type LinkSet = ParallelSet<Link2, 4>;

// Avoid NonNull<()> in FFI; see PR:
// https://github.com/mozilla/cbindgen/pull/1098
pub type LinkId2 = std::ptr::NonNull<c_void>;
