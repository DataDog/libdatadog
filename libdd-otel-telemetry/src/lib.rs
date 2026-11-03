// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared OpenTelemetry metrics aggregation and OTLP export for Datadog tracers.
//!
//! This crate centralizes aggregation, collection, OTLP encoding, and export without linking the
//! OpenTelemetry Rust SDK. It uses libdatadog's runtime, transport, and vendored OTLP protobuf
//! primitives so host tracers do not gain another SDK dependency.
//!
//! # Design principle: primitives only
//!
//! [`OtelMetricsAggregator`]'s public API never accepts or returns an OTel SDK object. Consumers
//! register synchronous instruments once (getting back an opaque [`InstrumentId`]) and push
//! primitive values (`f64` plus OpenTelemetry API [`KeyValue`] attributes). Observable instruments
//! instead register a host adapter callback that returns the same primitive measurements. The
//! native metrics reader invokes that adapter during collection, so every language can use the
//! same scheduling and aggregation mechanism.
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
mod attribute;
mod config;
mod error;
mod exporter;
mod instrument;
mod resource;
mod wire;

pub use aggregator::{ExportCounters, OtelMetricsAggregator, OtelMetricsAggregatorBuilder};
pub use attribute::{AttributeArray, AttributeValue, KeyValue};
pub use config::{OtlpExporterConfig, OtlpProtocol, Temporality, parse_otlp_headers};
pub use error::{BuildWarning, OtelMetricsError};
pub use exporter::{DatadogMetricExporter, build_datadog_metric_exporter};
pub use instrument::{
    InstrumentDescriptor, InstrumentId, InstrumentKind, ObservableCallback, ObservableMeasurement,
};
pub use resource::ResourceBuilder;
