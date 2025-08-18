// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod collections;
pub mod datatypes;

mod compressor;
mod error;
pub mod pprof_builder;
pub mod string_writer;

pub use compressor::*;
pub use error::*;
pub use pprof_builder::*;
pub use string_writer::*;

// todo: can we remove this?
#[derive(Copy, Clone, Default, Debug, Eq, PartialEq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub struct ManagedStringId {
    pub value: u32,
}
