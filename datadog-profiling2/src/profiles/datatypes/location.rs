// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::ParallelSet;
use crate::profiles::datatypes::{OptionalFunctionId2, OptionalMappingId2};
use std::ffi::c_void;
use std::ptr::null_mut;

/// A representation of a location that is an intersection of the Otel and
/// Pprof representations. Omits some fields to save space because Datadog
/// doesn't use them in any way. Additionally, Datadog only ever sets one Line,
/// so it's not a Vec.
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Location2 {
    pub address: u64,
    pub mapping_id: OptionalMappingId2,
    pub line: Line2,
}
// todo: Ivo thinks it would be nicer if we kept the API more similar to what
//       was there before, something like:
// pub struct Location {
//     pub address: u64,
//     pub mapping_id: OptionalMappingId,
//     pub function_id: FunctionId,
//     pub line_number: i64,
// }

impl Default for Location2 {
    fn default() -> Location2 {
        Location2 {
            address: 0,
            mapping_id: null_mut(),
            line: Line2::default(),
        }
    }
}

// Avoid NonNull<()> in FFI; see PR:
// https://github.com/mozilla/cbindgen/pull/1098
pub type LocationId2 = std::ptr::NonNull<c_void>;

/// A representation of a line plus function. It omits the column because it's
/// not used by Datadog.
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Line2 {
    pub line_number: i64,
    pub function_id: OptionalFunctionId2,
}

impl Default for Line2 {
    fn default() -> Line2 {
        Line2 {
            line_number: 0,
            function_id: null_mut(),
        }
    }
}

pub type LocationSet = ParallelSet<Location2, 4>;
