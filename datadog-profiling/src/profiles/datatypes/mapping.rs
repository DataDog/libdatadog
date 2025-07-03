// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::{ParallelSet, StringId};
use std::ffi::c_void;

/// A representation of a mapping that is an intersection of the Otel and Pprof
/// representations. Omits boolean attributes because Datadog doesn't use them
/// in any way.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Mapping {
    pub memory_start: u64,
    pub memory_limit: u64,
    pub file_offset: u64,
    pub filename: StringId,
    pub build_id: StringId, // missing in Otel, is it made into an attribute?
}

// Avoid NonNull<()> in FFI; see PR:
// https://github.com/mozilla/cbindgen/pull/1098
pub type MappingId = std::ptr::NonNull<c_void>;

pub type MappingSet = ParallelSet<Mapping, 2>;
