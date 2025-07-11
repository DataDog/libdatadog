// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod compressor;
mod endpoint_stats;
mod endpoints;
mod error;
mod labels_set;
mod profile_builder;
mod samples;
mod stack_trace_set;

pub use compressor::*;
pub use endpoint_stats::*;
pub use endpoints::*;
pub use error::*;
pub use labels_set::*;
pub use profile_builder::*;
pub use samples::*;
pub use stack_trace_set::*;

// todo: can we remove this?
#[derive(Copy, Clone, Default, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct ManagedStringId {
    pub value: u32,
}
