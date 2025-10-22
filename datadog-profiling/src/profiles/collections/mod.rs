// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod arc;
mod error;
// mod ffi_parallel_string_set;
mod parallel_set;
mod parallel_slice_set;
mod parallel_string_set;
mod set;
mod sharded;
mod slice_set;
mod string_set;
mod thin_str;

pub type SetHasher = core::hash::BuildHasherDefault<rustc_hash::FxHasher>;

pub use arc::*;
pub use error::*;
// pub use ffi_parallel_string_set::*;
pub use parallel_set::*;
pub use parallel_slice_set::*;
pub use parallel_string_set::*;
pub use set::*;
pub use sharded::*;
pub use slice_set::*;
pub use string_set::*;
pub use thin_str::*;
