// Unless explicitly stated otherwise all files in this repository are licensed under the Apache
// License Version 2.0. This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::{DslString, ProbeCondition, ProbeValue};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[repr(C)]
pub struct CaptureConfiguration {
    #[serde(default = "default_max_reference_depth")]
    pub max_reference_depth: u32,
    #[serde(default = "default_max_collection_size")]
    pub max_collection_size: u32,
    #[serde(default = "default_max_length")]
    pub max_length: u32,
    #[serde(default = "default_max_field_count")]
    pub max_field_count: u32,
}

fn default_max_reference_depth() -> u32 {
    3
}
fn default_max_collection_size() -> u32 {
    100
}
fn default_max_length() -> u32 {
    255
}
fn default_max_field_count() -> u32 {
    20
}

impl Default for CaptureConfiguration {
    fn default() -> Self {
        CaptureConfiguration {
            max_reference_depth: default_max_reference_depth(),
            max_collection_size: default_max_collection_size(),
            max_length: default_max_length(),
            max_field_count: default_max_field_count(),
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
    pub capture: CaptureConfiguration,
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
    pub lines: Vec<u32>,
    pub in_body_location: InBodyLocation,
}

#[repr(C)]
#[derive(Deserialize, Copy, Clone, Debug)]
#[serde(rename_all = "UPPERCASE")]
pub enum EvaluateAt {
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
