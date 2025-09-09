// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::ParallelSet;
use crate::profiles::datatypes::{OptionalFunctionId, OptionalMappingId};
use std::ffi::c_void;

/// A representation of a location that is an intersection of the Otel and
/// Pprof representations. Omits some fields to save space because Datadog
/// doesn't use them in any way. Additionally, Datadog only ever sets one Line,
/// so it's not a Vec.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Location {
    pub address: u64,
    pub mapping_id: OptionalMappingId,
    pub line: Line,
}

// Avoid NonNull<()> in FFI; see PR:
// https://github.com/mozilla/cbindgen/pull/1098
pub type LocationId = std::ptr::NonNull<c_void>;

/// A representation of a line plus function. It omits the column because it's
/// not used by Datadog.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Line {
    pub line_number: i64,
    pub function_id: OptionalFunctionId,
}

pub type LocationSet = ParallelSet<Location, 4>;
