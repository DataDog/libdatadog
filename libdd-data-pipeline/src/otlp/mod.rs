// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod config;
pub mod exporter;
pub mod metrics;

#[cfg(not(target_arch = "wasm32"))]
pub mod grpc_exporter;

pub use config::{OtlpMetricsConfig, OtlpProtocol, OtlpTraceConfig};
#[allow(unused_imports)]
pub use exporter::send_otlp_traces_http;
pub use libdd_trace_utils::otlp_encoder::{map_traces_to_otlp, OtlpResourceInfo};
pub use metrics::OtlpStatsExporter;

#[cfg(not(target_arch = "wasm32"))]
pub use config::OtlpGrpcTraceConfig;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use grpc_exporter::{
    build_grpc_transport, send_otlp_traces_grpc, GrpcExportError, OtlpGrpcTransport,
};
