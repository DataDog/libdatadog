// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared OpenTelemetry metrics aggregation and OTLP export for Datadog tracers.
//!
//! Every `dd-trace-xx` library today depends on and configures its own copy of the community
//! OpenTelemetry **SDK** (aggregation, views, OTLP encoding) to support `ddtrace`'s OTel
//! metrics/logs bridge. This crate centralizes that implementation in one place, built on top of
//! upstream `opentelemetry_sdk`/`opentelemetry-otlp` — this is the only crate in the Datadog
//! tracer ecosystem meant to depend on those packages going forward.
//!
//! # Design principle: primitives only
//!
//! [`OtelMetricsAggregator`]'s public API never accepts or returns an OTel SDK object, and never
//! accepts a callback/closure. Consumers register an instrument once (getting back an opaque
//! [`InstrumentId`]) and then push primitive values (`f64` + string key/value attributes) for it.
//! This holds even for observable/async instruments (`ObservableGauge`, `ObservableCounter`):
//! the host language keeps ownership of *when* to evaluate a user-registered callback (only it
//! can execute that closure), and pushes the resolved value the same way a synchronous
//! instrument would. The aggregator does not need to know which kind produced a given value.
//!
//! Keeping the boundary primitives-only is deliberate, not incidental: it's what makes this
//! crate usable from Rust (dd-trace-rs, today) and, later, from other languages via a C-ABI
//! `-ffi` layer (dd-trace-py via PyO3 first; Node/Ruby/PHP after) without redesigning the core.
//!
//! # What this crate does not do
//!
//! - It does not decide whether to configure OTel support at all — "defer to a user's own OTel SDK
//!   setup if they've already configured one" is inherently host-language/SDK-specific and stays
//!   the host tracer's responsibility.
//! - It does not read any tracer's configuration type directly — callers extract primitives from
//!   their own config and pass them to the builder.
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::panic))]

mod aggregator;
mod config;
mod error;
#[cfg(any(feature = "grpc", feature = "http"))]
mod exporter;
mod instrument;
mod resource;

pub use aggregator::{ExportCounters, OtelMetricsAggregator, OtelMetricsAggregatorBuilder};
pub use config::{parse_otlp_headers, OtlpExporterConfig, OtlpProtocol, Temporality};
pub use error::{BuildWarning, OtelMetricsError};
#[cfg(any(feature = "grpc", feature = "http"))]
pub use exporter::{build_datadog_metric_exporter, DatadogMetricExporter};
pub use instrument::{InstrumentDescriptor, InstrumentId, InstrumentKind};
pub use resource::ResourceBuilder;
