// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! # Encoder layout & naming convention
//!
//! ```text
//! msgpack_encoder/
//! ├── v04/
//! │   ├── mod.rs        // public API + payload-level helpers
//! │   ├── span_v04.rs   // v0.4 in-memory Span  → v0.4 wire (native)
//! │   └── span_v1.rs    // v1  in-memory Span  → v0.4 wire (downgrade)
//! └── v1/
//!     ├── mod.rs
//!     ├── span_v04.rs   // v0.4 in-memory Span  → V1 wire (upgrade)
//!     └── span_v1.rs    // v1  in-memory Span  → V1 wire (native)
//! ```
//!
//! - **Module (`v04`/`v1`) = output wire format.**
//! - **File suffix (`_v04`/`_v1`) = input span type.**
//! - **Public functions carry a `_from_<input>` suffix**, so a caller reads the *output* from the
//!   module path and the *input* from the function name:
//!
//!   | Module | Function | Input → Output |
//!   |--------|----------|----------------|
//!   | `v04::` | `to_vec_from_v04`, `write_to_slice_from_v04`, `to_encoded_byte_len_from_v04` | v04 → v0.4 (native) |
//!   | `v04::` | `to_vec_from_v1`, `write_to_slice_from_v1`, `to_encoded_byte_len_from_v1`   | v1  → v0.4 (downgrade) |
//!   | `v1::`  | `to_vec_from_v04`, `write_to_slice_from_v04`, `to_encoded_byte_len_from_v04` | v04 → V1  (upgrade) |
//!   | `v1::`  | `to_vec_from_v1`, `write_to_slice_from_v1`, `to_encoded_byte_len_from_v1`     | v1  → V1  (native) |

pub mod v04;
pub mod v1;

use rmp::encode::ValueWriteError;
use std::convert::Infallible;

/// Flatten `ValueWriteError<Infallible>` (uninhabited because both variants wrap
/// `Infallible`) into the bare `Infallible` so callers can use
/// [`libdd_common::ResultInfallibleExt`].
#[inline(always)]
pub(crate) fn flatten_value_write_infallible(err: ValueWriteError<Infallible>) -> Infallible {
    match err {
        ValueWriteError::InvalidMarkerWrite(never) | ValueWriteError::InvalidDataWrite(never) => {
            never
        }
    }
}

/// A writer that counts bytes without storing them, used to compute encoded payload size.
pub(crate) struct CountLength(u32);

impl std::io::Write for CountLength {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_all(buf)?;
        Ok(buf.len())
    }

    #[inline]
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    #[inline]
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.0 += buf.len() as u32;
        Ok(())
    }
}
