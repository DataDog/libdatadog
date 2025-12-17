// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::datatypes::{FunctionId2, MappingId2, StringId2};

#[derive(Copy, Clone, Debug, Default)]
#[repr(C)]
pub struct Location2 {
    pub mapping: MappingId2,
    pub function: FunctionId2,

    /// The instruction address for this location, if available.  It
    /// should be within [Mapping.memory_start...Mapping.memory_limit]
    /// for the corresponding mapping. A non-leaf address may be in the
    /// middle of a call instruction. It is up to display tools to find
    /// the beginning of the instruction if necessary.
    pub address: u64,
    pub line: i64,
}

#[derive(Copy, Clone, Debug, Default)]
pub struct Label<'a> {
    pub key: StringId2,

    /// At most one of `.str` and `.num` should not be empty.
    pub str: &'a str,
    pub num: i64,

    /// Should only be present when num is present.
    /// Specifies the units of num.
    /// Use arbitrary string (for example, "requests") as a custom count unit.
    /// If no unit is specified, consumer may apply heuristic to deduce the unit.
    /// Consumers may also  interpret units like "bytes" and "kilobytes" as memory
    /// units and units like "seconds" and "nanoseconds" as time units,
    /// and apply appropriate unit conversions to these.
    pub num_unit: &'a str,
}

impl<'a> Label<'a> {
    pub const fn str(key: StringId2, str: &'a str) -> Label<'a> {
        Label {
            key,
            str,
            num: 0,
            num_unit: "",
        }
    }

    pub const fn num(key: StringId2, num: i64, num_unit: &'a str) -> Label<'a> {
        Label {
            key,
            str: "",
            num,
            num_unit,
        }
    }
}
