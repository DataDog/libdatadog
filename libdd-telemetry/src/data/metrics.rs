// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common::tag::Tag;
use serde::{Deserialize, Serialize};

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

#[derive(Serialize, Debug)]
#[serde(untagged)]
pub enum SerializedSketch {
    Bytes { sketch: Vec<u8> },
    B64 { sketch_b64: String },
}

#[derive(
    Serialize,
    Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    strum_macros::Display,
    strum_macros::EnumIter,
    strum_macros::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
#[repr(C)]
pub enum MetricNamespace {
    Tracers,
    Profilers,
    Rum,
    Appsec,
    IdePlugins,
    /// Superseded by [`MetricNamespace::Debugger`]. Kept so existing FFI discriminants stay stable.
    LiveDebugger,
    Iast,
    General,
    Telemetry,
    Apm,
    Sidecar,
    Civisibility,
    Mlobs,
    Ddtraceapi,
    AiGuard,
    /// Metrics from the debugger products (Live Debugger, Dynamic Instrumentation, Exception Replay, etc.).
    Debugger,
}

#[derive(
    Serialize,
    Deserialize,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    strum_macros::Display,
    strum_macros::EnumIter,
    strum_macros::IntoStaticStr,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
#[repr(C)]
pub enum MetricType {
    Gauge,
    Count,
    Distribution,
    Rate,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debugger_namespace_name() {
        assert_eq!(
            serde_json::to_value(MetricNamespace::Debugger).unwrap(),
            "debugger"
        );
        assert_eq!(MetricNamespace::Debugger.to_string(), "debugger");
    }
}
