// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Agentless APM trace export for libdatadog.
//!
//! When an agentless endpoint is configured via
//! [`crate::trace_exporter::TraceExporterBuilder::set_agentless_endpoint`], the
//! trace exporter sends APM trace spans directly to the Datadog HTTP intake
//! instead of to the local Datadog Agent.
//!
//! ## Differences from the regular agent export
//!
//! - **Transport**: `POST` to the public HTTP trace intake (default `https://public-trace-http-intake.logs.{DD_SITE}/v1/input`,
//!   or a custom URL) using `dd-api-key` auth, instead of msgpack to the local agent's
//!   `/v0.4/traces`. The host language resolves the URL from `DD_SITE` and supplies the API key;
//!   the exporter reads no environment variables.
//! - **Encoding**: JSON (see [`libdd_trace_utils::agentless_encoder`]) instead of msgpack v04. See
//!   that module for the payload-shape differences.
//! - **Retries**: up to 3 attempts with exponential backoff starting at 1 s and no cap (the agent
//!   path uses its own strategy).
//! - **Mutual exclusion with OTLP**: if both an OTLP and an agentless endpoint are configured on
//!   the builder, OTLP wins and the agentless config is silently dropped with a warning at build
//!   time.

pub(crate) mod config;
pub(crate) mod exporter;

pub use config::AgentlessTraceConfig;
pub use exporter::send_agentless_traces_http;
