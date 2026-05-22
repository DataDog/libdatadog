// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

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
