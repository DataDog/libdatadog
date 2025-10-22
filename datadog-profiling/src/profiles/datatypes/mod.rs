// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module holds datatypes which map roughly to otel and pprof messages.
//! There are some differences for efficiency or to accommodate both formats.
//! There are some novel types such as [`ScratchPad`] as well.

mod function;
mod mapping;
mod profiles_dictionary;
mod value_type;

pub use function::*;
pub use mapping::*;
pub use profiles_dictionary::*;
pub use value_type::*;
