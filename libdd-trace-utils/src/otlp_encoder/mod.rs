// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP HTTP/JSON encoder: maps Datadog spans to ExportTraceServiceRequest.

pub mod json_types;
pub mod mapper;

pub use mapper::map_traces_to_otlp;

/// Tracer-level attributes used to populate the OTLP Resource on export.
///
/// These are the fields from the tracer's configuration that map to OTLP Resource attributes
/// (service.name, deployment.environment.name, service.version, telemetry.sdk.*, runtime-id).
/// Callers should build this from their own tracer metadata struct.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct OtlpResourceInfo {
    pub service: String,
    pub env: String,
    pub app_version: String,
    pub language: String,
    pub tracer_version: String,
    pub runtime_id: String,
}
