// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod error;
mod set;
mod slice_set;
mod string_set;
mod thin_str;

pub type SetHasher = core::hash::BuildHasherDefault<rustc_hash::FxHasher>;

pub use error::*;
pub use set::*;
pub use slice_set::*;
pub use string_set::*;
pub use thin_str::*;
