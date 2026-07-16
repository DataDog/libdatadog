// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "std")]
use libdd_common::tag::Tag;
#[cfg(feature = "std")]
use serde::Serialize;

#[cfg(feature = "std")]
#[derive(Serialize, Debug)]
pub struct Serie {
    pub namespace: MetricNamespace,
    pub metric: String,
    pub points: Vec<(u64, f64)>,
    pub tags: Vec<Tag>,
    pub common: bool,
    #[serde(rename = "type")]
    pub _type: MetricType,
    pub interval: u64,
}

#[cfg(feature = "std")]
#[derive(Serialize, Debug)]
pub struct Distribution {
    pub namespace: MetricNamespace,
    pub metric: String,
    pub tags: Vec<Tag>,
    #[serde(flatten)]
    pub sketch: SerializedSketch,
    pub common: bool,
    pub interval: u64,
    #[serde(rename = "type")]
    pub _type: MetricType,
}

#[cfg(feature = "std")]
#[derive(Serialize, Debug)]
#[serde(untagged)]
pub enum SerializedSketch {
    Bytes { sketch: Vec<u8> },
    B64 { sketch_b64: String },
}

#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum MetricNamespace {
    Tracers,
    Profilers,
    Rum,
    Appsec,
    IdePlugins,
    LiveDebugger,
    Iast,
    General,
    Telemetry,
    Apm,
    Sidecar,
}

#[cfg(feature = "signal-safe")]
impl MetricNamespace {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Tracers => "tracers",
            Self::Profilers => "profilers",
            Self::Rum => "rum",
            Self::Appsec => "appsec",
            Self::IdePlugins => "ide_plugins",
            Self::LiveDebugger => "live_debugger",
            Self::Iast => "iast",
            Self::General => "general",
            Self::Telemetry => "telemetry",
            Self::Apm => "apm",
            Self::Sidecar => "sidecar",
        }
    }
}

#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "std", serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub enum MetricType {
    Gauge,
    Count,
    Distribution,
}

#[cfg(feature = "signal-safe")]
impl MetricType {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Gauge => "gauge",
            Self::Count => "count",
            Self::Distribution => "distribution",
        }
    }
}
