// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

/// Opaque handle to an instrument registered on a [`crate::OtelMetricsAggregator`].
///
/// This is the only thing consumers hold onto for an instrument — never the underlying SDK
/// object — so the handle is a plain primitive and can cross an FFI boundary unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstrumentId(pub u64);

/// A primitive measurement returned by a host-language observable callback.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservableMeasurement {
    pub value: f64,
    pub attributes: Vec<(String, String)>,
}

impl ObservableMeasurement {
    pub fn new(value: f64, attributes: Vec<(String, String)>) -> Self {
        Self { value, attributes }
    }
}

/// A callback invoked by the native metrics reader during collection.
pub type ObservableCallback = Arc<dyn Fn() -> Vec<ObservableMeasurement> + Send + Sync + 'static>;

/// The kind of instrument being registered.
///
/// Observable instruments are registered with a host callback which the native reader invokes
/// during collection. Synchronous instruments receive primitive values through `record_*` and
/// `observe_*` methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstrumentKind {
    Counter,
    UpDownCounter,
    Histogram,
    ObservableGauge,
    ObservableCounter,
    ObservableUpDownCounter,
}

/// Metadata needed to create the underlying instrument once, at registration time.
///
/// The `meter_*` fields carry the OpenTelemetry instrumentation scope the instrument belongs to
/// (the name/version/schema_url the host passed to `get_meter`). The aggregator creates one SDK
/// `Meter` per distinct scope so exported metrics keep the host's scope rather than a single
/// crate-internal one.
#[derive(Debug, Clone)]
pub struct InstrumentDescriptor {
    pub name: String,
    pub kind: InstrumentKind,
    pub unit: Option<String>,
    pub description: Option<String>,
    pub meter_name: String,
    pub meter_version: Option<String>,
    pub meter_schema_url: Option<String>,
}

impl InstrumentDescriptor {
    pub fn new(name: impl Into<String>, kind: InstrumentKind) -> Self {
        Self {
            name: name.into(),
            kind,
            unit: None,
            description: None,
            meter_name: "libdd-otel-telemetry".to_string(),
            meter_version: None,
            meter_schema_url: None,
        }
    }

    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets the instrumentation scope (the host's `get_meter` identity) this instrument belongs to.
    pub fn with_scope(
        mut self,
        meter_name: impl Into<String>,
        meter_version: Option<String>,
        meter_schema_url: Option<String>,
    ) -> Self {
        self.meter_name = meter_name.into();
        self.meter_version = meter_version;
        self.meter_schema_url = meter_schema_url;
        self
    }
}
