// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::ParallelSet;

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Link {
    trace_id: u64, // Otel is 16-bytes, not using that yet.
    span_id: u64,
}

pub type LinkSet = ParallelSet<Link, 4>;
