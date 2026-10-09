// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Outcomes for best-effort flagevaluation submission.

use std::io;

/// Local submission outcomes, not metric names or delivery guarantees.
/// Do not report a rejected observation recursively through the EVP path.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfeSubmissionStatus {
    /// Accepted by the local transport, not necessarily delivered to the sidecar or intake.
    Accepted,
    /// Absent, closed, poisoned, or failed transport. Recovery belongs to lifecycle code.
    Unavailable,
    /// Rejected by shared load shedding, pending configuration, or socket backpressure.
    WouldBlock,
    /// Invalid required input for the single-observation interface.
    InvalidInput,
    /// The observation still exceeds a packet limit after any reduced-event retry.
    PayloadTooLarge,
}

impl From<io::Error> for FfeSubmissionStatus {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::WouldBlock => Self::WouldBlock,
            io::ErrorKind::FileTooLarge => Self::PayloadTooLarge,
            #[cfg(unix)]
            _ if error.raw_os_error() == Some(libc::EMSGSIZE) => Self::PayloadTooLarge,
            _ => Self::Unavailable,
        }
    }
}
