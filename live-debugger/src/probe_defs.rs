// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use serde::Deserialize;
use crate::{DslString, ProbeCondition, ProbeValue};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[repr(C)]
pub struct Capture {
    pub max_reference_depth: u32,
    pub max_collection_size: u32,
    pub max_length: u32,
    pub max_field_count: u32,
}

impl Default for Capture {
    fn default() -> Self {
        Capture {
            max_reference_depth: 3,
            max_collection_size: 100,
            max_length: 255,
            max_field_count: 20,
        }
    }
}

#[repr(C)]
#[derive(Deserialize, Copy, Clone, Debug)]
#[serde(rename_all = "UPPERCASE")]
pub enum MetricKind {
    Count,
    Gauge,
    Histogram,
    Distribution,
}

#[derive(Debug)]
pub struct MetricProbe {
    pub kind: MetricKind,
    pub name: String,
    pub value: ProbeValue, // May be Value::Null
}

#[repr(C)]
#[derive(Deserialize, Copy, Clone, Debug)]
#[serde(rename_all = "UPPERCASE")]
pub enum SpanProbeTarget {
    Active,
    Root,
}

#[derive(Debug)]
pub struct SpanProbeDecoration {
    pub condition: ProbeCondition,
    pub tags: Vec<(String, DslString)>,
}

#[derive(Debug)]
pub struct LogProbe {
    pub segments: DslString,
    pub when: ProbeCondition,
    pub capture: Capture,
    pub capture_snapshot: bool,
    pub sampling_snapshots_per_second: u32,
}

#[derive(Debug)]
pub struct SpanProbe {}

#[derive(Debug)]
pub struct SpanDecorationProbe {
    pub target: SpanProbeTarget,
    pub decorations: Vec<SpanProbeDecoration>,
}

#[derive(Debug)]
pub enum ProbeType {
    Metric(MetricProbe),
    Log(LogProbe),
    Span(SpanProbe),
    SpanDecoration(SpanDecorationProbe),
}

#[repr(C)]
#[derive(Deserialize, Copy, Clone, Debug)]
#[serde(rename_all = "UPPERCASE")]
pub enum InBodyLocation {
    None,
    Start,
    End,
}

#[derive(Debug)]
pub struct ProbeTarget {
    pub type_name: Option<String>,
    pub method_name: Option<String>,
    pub source_file: Option<String>,
    pub signature: Option<String>,
    pub lines: Vec<String>,
    pub in_body_location: InBodyLocation,
}

#[repr(C)]
#[derive(Deserialize, Copy, Clone, Debug)]
#[serde(rename_all = "UPPERCASE")]
pub enum EvaluateAt {
    Default,
    Entry,
    Exit,
}

#[derive(Debug)]
pub struct Probe {
    pub id: String,
    pub version: u64,
    pub language: Option<String>,
    pub tags: Vec<String>,
    pub target: ProbeTarget, // "where" is rust keyword
    pub evaluate_at: EvaluateAt,
    pub probe: ProbeType,
}

#[derive(Debug, Default, Deserialize)]
pub struct FilterList {
    pub package_prefixes: Vec<String>,
    pub classes: Vec<String>,
}

#[derive(Debug)]
pub struct ServiceConfiguration {
    pub id: String,
    pub allow: FilterList,
    pub deny: FilterList,
    pub sampling_snapshots_per_second: u32,
}

#[derive(Debug)]
pub enum LiveDebuggingData {
    Probe(Probe),
    ServiceConfiguration(ServiceConfiguration),
}
