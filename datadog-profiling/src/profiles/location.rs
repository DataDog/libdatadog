// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::ProfileId;
use crate::profiles::collections::StringOffset;

/// A representation of a location that is an intersection of the Otel and
/// Pprof representations. Omits some fields to save space because Datadog
/// doesn't use them in any way. Additionally, Datadog only ever sets one Line,
/// so it's not a Vec, and it's folded into Location to avoid some padding.
#[repr(C)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Location {
    address: u64,
    line_number: i64,
    mapping_id: ProfileId,
    function_id: ProfileId,
    filename: StringOffset,
}
