// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module holds datatypes which map roughly to otel and pprof messages.
//! There are some differences for efficiency or to accommodate both formats.
//! There are some novel types such as [`ScratchPad`] as well.

mod attribute;
mod endpoint_tracker;
mod function;
mod link;
mod location;
mod mapping;
mod profile;
mod profiles_dictionary;
mod sample;
mod scratchpad;
mod stack;
mod value_type;

pub use attribute::*;
pub use endpoint_tracker::*;
pub use function::*;
pub use link::*;
pub use location::*;
pub use mapping::*;
pub use profile::*;
pub use profiles_dictionary::*;
pub use sample::*;
pub use scratchpad::*;
pub use stack::*;
pub use value_type::*;
