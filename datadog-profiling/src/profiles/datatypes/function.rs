// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::{ParallelSet, StringId};
use std::ffi::c_void;

/// A representation of a function that is an intersection of the Otel and
/// Pprof representations. Omits the start line to save space because Datadog
/// doesn't use this in any way.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Function {
    pub name: StringId,
    pub system_name: StringId,
    pub file_name: StringId,
}

// Avoid NonNull<()> in FFI; see PR:
// https://github.com/mozilla/cbindgen/pull/1098
pub type FunctionId = std::ptr::NonNull<c_void>;

pub type FunctionSet = ParallelSet<Function, 4>;
