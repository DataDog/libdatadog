// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Local admission outcomes for best-effort flagevaluation submission.

/// A successful readiness check reserves nothing; submission rechecks admission.
/// Count a rejected observation once, not once for each check. These outcomes
/// are not metric names and must not be reported through the rejected EVP path.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfeSubmissionStatus {
    /// Advisory only; no snapshot or message has been accepted.
    Ready,
    /// Accepted by the local transport, not necessarily delivered to the sidecar or intake.
    Accepted,
    /// Absent, closed, or poisoned transport. Recovery belongs to ordinary lifecycle code.
    Unavailable,
    /// Another caller owns the sender; no waiting was attempted.
    Busy,
    /// Shared outstanding-message ceiling reached before snapshot construction.
    QueueFull,
    /// Rejected by the shared low-priority shedding policy before snapshot construction.
    LoadShed,
    /// Required priority messages could not be sent first.
    PriorityPending,
    /// The observation could not be accepted immediately after admission.
    WouldBlock,
    /// Invalid required input or a request other than one FFE observation.
    InvalidInput,
    /// Exceeds the existing IPC packet ceiling; the connection remains usable.
    PayloadTooLarge,
    /// Request encoding failed before any bytes were sent.
    EncodingError,
}
