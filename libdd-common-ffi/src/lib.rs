// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

extern crate alloc;

#[cfg(feature = "std")]
mod error;

#[cfg(feature = "std")]
pub mod array_queue;
pub mod cstr;
#[cfg(feature = "std")]
pub mod endpoint;
#[cfg(feature = "std")]
pub mod handle;
#[cfg(feature = "std")]
pub mod option;
#[cfg(feature = "std")]
pub mod result;
pub mod slice;
#[cfg(feature = "std")]
pub mod slice_mut;
#[cfg(feature = "std")]
pub mod string;
#[cfg(feature = "std")]
pub mod tags;
#[cfg(feature = "std")]
pub mod timespec;
#[cfg(feature = "std")]
pub mod utils;
#[cfg(feature = "std")]
pub mod vec;

pub use cstr::*;
pub use slice::{CharSlice, Slice};

#[cfg(feature = "std")]
pub use error::*;
#[cfg(feature = "std")]
pub use handle::*;
#[cfg(feature = "std")]
pub use option::*;
#[cfg(feature = "std")]
pub use result::*;
#[cfg(feature = "std")]
pub use slice_mut::MutSlice;
#[cfg(feature = "std")]
pub use string::*;
#[cfg(feature = "std")]
pub use timespec::*;
#[cfg(feature = "std")]
pub use vec::Vec;
