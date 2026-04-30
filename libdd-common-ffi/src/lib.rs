// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

extern crate alloc;

pub mod cstr;
pub mod slice;
pub mod vec;

pub use cstr::*;
pub use slice::{CharSlice, Slice};
pub use vec::Vec;

pub mod array_queue;
pub mod endpoint;
mod error;
pub mod handle;
pub mod option;
pub mod result;
pub mod slice_mut;
pub mod string;
pub mod tags;
pub mod timespec;
pub mod utils;

#[cfg(feature = "std")]
pub use {error::*, handle::*, option::*, result::*, slice_mut::MutSlice, string::*, timespec::*};
