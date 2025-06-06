// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

mod error;

pub mod array_queue;
pub mod cstr;
pub mod endpoint;
pub mod handle;
pub mod option;
pub mod result;
pub mod slice;
pub mod slice_mut;
pub mod string;
pub mod tags;
pub mod timespec;
pub mod utils;
pub mod vec;

pub use cstr::*;
pub use error::*;
pub use handle::*;
pub use option::*;
pub use result::*;
pub use slice::{CharSlice, Slice};
pub use slice_mut::MutSlice;
pub use string::*;
pub use timespec::*;
pub use vec::Vec;
