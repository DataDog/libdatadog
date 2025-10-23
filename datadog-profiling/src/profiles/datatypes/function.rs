// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::{ParallelSet, StringRef};

/// A representation of a function that is an intersection of the Otel and
/// Pprof representations. Omits the start line to save space because Datadog
/// doesn't use this in any way.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
#[repr(C)]
pub struct Function {
    pub name: StringRef,
    pub system_name: StringRef,
    pub file_name: StringRef,
}

pub use crate::api2::FunctionId2;

pub type FunctionSet = ParallelSet<Function, 4>;
