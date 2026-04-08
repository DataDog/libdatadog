// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "telemetry")]
pub(crate) use libdd_telemetry::worker::TelemetryWorker;

#[cfg(not(feature = "telemetry"))]
#[derive(Debug)]
pub(crate) struct TelemetryWorker {}

#[cfg(not(feature = "telemetry"))]
impl libdd_common::worker::Worker for TelemetryWorker {
    async fn run(&mut self) {}
}
