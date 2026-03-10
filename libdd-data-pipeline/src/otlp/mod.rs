// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP trace export for libdatadog.
//!
//! When `OTEL_TRACES_EXPORTER=otlp` is set, the trace exporter sends traces in OTLP HTTP/JSON
//! format to the configured endpoint instead of the Datadog agent. See
//! [`config::otlp_trace_config_from_env`] and [`config::env_keys`] for environment variable names
//! and precedence.
//!
//! ## Sampling
//!
//! By default, the exporter does not apply its own sampling: it exports every trace it receives
//! from the tracer. The tracer (e.g. dd-trace-py) is responsible for inheriting the sampling
//! decision from the distributed trace context; when no decision is present, the tracer typically
//! uses 100% (always on).
//!
//! ## Partial flush
//!
//! For the POC, partial flush is disabled. The tracer should only invoke the exporter when all
//! spans from a local trace are closed (i.e. send complete trace chunks). This crate does not
//! buffer or flush partially—it exports whatever trace chunks it receives.

pub mod config;
pub mod exporter;

pub use config::{otlp_trace_config_from_env, OtlpTraceConfig};
pub use exporter::send_otlp_traces_http;
pub use libdd_trace_utils::otlp_encoder::{map_traces_to_otlp, OtlpResourceInfo};
