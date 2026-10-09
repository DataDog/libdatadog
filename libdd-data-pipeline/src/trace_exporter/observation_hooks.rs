// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The only points where the exporter's send path consults external observations.
//!
//! Without the `external-observations` feature these are no-ops that report that no
//! observation scope is active, so the send path keeps delivering to native telemetry.
//! Removing the feature means deleting this file and its call sites.

#[cfg(feature = "external-observations")]
pub(super) use super::observations::{record_dropped, record_payload};

#[cfg(not(feature = "external-observations"))]
pub(super) fn record_payload(
    _payload: impl FnOnce() -> super::payload_telemetry::SendPayloadTelemetry,
) -> bool {
    false
}

#[cfg(not(feature = "external-observations"))]
pub(super) fn record_dropped(_p0_chunks: usize, _trace_filter_chunks: usize) -> bool {
    false
}
