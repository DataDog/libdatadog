// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OS strings available with `alloc`, including without `std`.
//!
//! With `std`, these are the standard library's native types. Without `std`, they store
//! arbitrary bytes, as used by Unix-style C environments. UTF-8 conversion is explicit;
//! invalid bytes are preserved. The byte implementation does not provide Windows UTF-16
//! conversion APIs.
//!
//! ```
//! use libdd_common::ffi::{OsStr, OsString};
//!
//! let name = OsString::from("SERVICE");
//! let borrowed: &OsStr = name.as_ref();
//! assert_eq!(borrowed.as_encoded_bytes(), b"SERVICE");
//! assert_eq!(name.into_string().unwrap(), "SERVICE");
//! ```

#[cfg(any(not(feature = "std"), test))]
mod os_string;

#[cfg(not(feature = "std"))]
pub use os_string::{OsStr, OsString};
#[cfg(feature = "std")]
pub use std::ffi::{OsStr, OsString};
