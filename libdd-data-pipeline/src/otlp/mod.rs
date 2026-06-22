// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP trace export for libdatadog.
//!
//! When an OTLP endpoint is configured via
//! [`crate::trace_exporter::TraceExporterBuilder::set_otlp_endpoint`], the trace exporter sends
//! traces in OTLP HTTP format to that endpoint instead of the Datadog agent; the wire encoding
//! (JSON or protobuf) is selected via [`OtlpProtocol`]. The host language is responsible for
//! resolving the endpoint from its own configuration (e.g.
//! `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`).
//!
//! ## Sampling
//!
//! The exporter enforces the sampling decision already made by the tracer: unsampled chunks are
//! dropped via `drop_chunks` before export. It does not apply its own sampling policy. The tracer
//! (e.g. dd-trace-py) is responsible for inheriting the sampling decision from the distributed
//! trace context; when no decision is present, the tracer typically uses 100% (always on).
//!
//! ## Partial flush
//!
//! For the POC, partial flush is disabled. The tracer should only invoke the exporter when all
//! spans from a local trace are closed (i.e. send complete trace chunks). This crate does not
//! buffer or flush partially—it exports whatever trace chunks it receives.

pub(crate) mod config;
pub(crate) mod exporter;

// `OtlpProtocol` is the only public symbol: it is a parameter of the public
// `TraceExporterBuilder::set_otlp_protocol`. Everything else here (the parsed config, the
// resource-info/mapper helpers, the send entrypoint) is internal to this crate.
pub use config::OtlpProtocol;
pub(crate) use config::OtlpTraceConfig;
pub(crate) use exporter::send_otlp_traces_http;
pub(crate) use libdd_trace_utils::otlp_encoder::{map_traces_to_otlp, OtlpResourceInfo};
