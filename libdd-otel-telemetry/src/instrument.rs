// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Opaque handle to an instrument registered on a [`crate::TelemetryAggregator`].
///
/// This is the only thing consumers hold onto for an instrument — never the underlying SDK
/// object — so the handle is a plain primitive and can cross an FFI boundary unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstrumentId(pub u64);

/// The kind of instrument being registered.
///
/// The aggregator does not distinguish synchronous instruments (`Counter`, `Histogram`,
/// `UpDownCounter`) from observable/async ones (`ObservableGauge`, `ObservableCounter`) once
/// registered — both just receive resolved primitive values via `record_*`/`observe_*`. The host
/// language owns deciding *when* an observable instrument's callback runs.
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
#[derive(Debug, Clone)]
pub struct InstrumentDescriptor {
    pub name: String,
    pub kind: InstrumentKind,
    pub unit: Option<String>,
    pub description: Option<String>,
}

impl InstrumentDescriptor {
    pub fn new(name: impl Into<String>, kind: InstrumentKind) -> Self {
        Self {
            name: name.into(),
            kind,
            unit: None,
            description: None,
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
}
