// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use datadog_live_debugger::{
    Capture, DslString, EvaluateAt, InBodyLocation, MetricKind, ProbeCondition, ProbeValue,
    SpanProbeTarget,
};
use ddcommon_ffi::{CharSlice, Option};

#[repr(C)]
pub struct CharSliceVec<'a> {
    pub strings: *const CharSlice<'a>,
    pub string_count: usize,
}

impl<'a> Drop for CharSliceVec<'a> {
    fn drop(&mut self) {
        unsafe {
            Vec::from_raw_parts(
                self.strings as *mut CharSlice,
                self.string_count,
                self.string_count,
            )
        };
    }
}

impl<'a> From<&'a Vec<String>> for CharSliceVec<'a> {
    fn from(from: &'a Vec<String>) -> Self {
        let char_slices: Vec<CharSlice<'a>> = from.iter().map(|s| s.as_str().into()).collect();
        let new = CharSliceVec {
            strings: char_slices.as_ptr(),
            string_count: char_slices.len(),
        };
        std::mem::forget(char_slices);
        new
    }
}

#[repr(C)]
pub struct MetricProbe<'a> {
    pub kind: MetricKind,
    pub name: CharSlice<'a>,
    pub value: &'a ProbeValue,
}

impl<'a> From<&'a datadog_live_debugger::MetricProbe> for MetricProbe<'a> {
    fn from(from: &'a datadog_live_debugger::MetricProbe) -> Self {
        MetricProbe {
            kind: from.kind,
            name: from.name.as_str().into(),
            value: &from.value,
        }
    }
}

#[repr(C)]
pub struct LogProbe<'a> {
    pub segments: &'a DslString,
    pub when: &'a ProbeCondition,
    pub capture: &'a Capture,
    pub sampling_snapshots_per_second: u32,
}

impl<'a> From<&'a datadog_live_debugger::LogProbe> for LogProbe<'a> {
    fn from(from: &'a datadog_live_debugger::LogProbe) -> Self {
        LogProbe {
            segments: &from.segments,
            when: &from.when,
            capture: &from.capture,
            sampling_snapshots_per_second: from.sampling_snapshots_per_second,
        }
    }
}

#[repr(C)]
pub struct Tag<'a> {
    pub name: CharSlice<'a>,
    pub value: &'a DslString,
}

#[repr(C)]
pub struct SpanProbeDecoration<'a> {
    pub condition: &'a ProbeCondition,
    pub tags: *const Tag<'a>,
    pub tags_count: usize,
}

impl<'a> From<&'a datadog_live_debugger::SpanProbeDecoration> for SpanProbeDecoration<'a> {
    fn from(from: &'a datadog_live_debugger::SpanProbeDecoration) -> Self {
        let tags: Vec<_> = from
            .tags
            .iter()
            .map(|(name, value)| Tag {
                name: name.as_str().into(),
                value,
            })
            .collect();

        let new = SpanProbeDecoration {
            condition: &from.condition,
            tags: tags.as_ptr(),
            tags_count: tags.len(),
        };
        std::mem::forget(tags);
        new
    }
}

impl<'a> Drop for SpanProbeDecoration<'a> {
    fn drop(&mut self) {
        unsafe {
            Vec::from_raw_parts(
                self.tags as *mut CharSlice,
                self.tags_count,
                self.tags_count,
            )
        };
    }
}

#[repr(C)]
pub struct SpanDecorationProbe<'a> {
    pub target: SpanProbeTarget,
    pub decorations: *const SpanProbeDecoration<'a>,
    pub decorations_count: usize,
}

impl<'a> From<&'a datadog_live_debugger::SpanDecorationProbe> for SpanDecorationProbe<'a> {
    fn from(from: &'a datadog_live_debugger::SpanDecorationProbe) -> Self {
        let tags: Vec<_> = from.decorations.iter().map(Into::into).collect();
        let new = SpanDecorationProbe {
            target: from.target,
            decorations: tags.as_ptr(),
            decorations_count: tags.len(),
        };
        std::mem::forget(tags);
        new
    }
}

impl<'a> Drop for SpanDecorationProbe<'a> {
    fn drop(&mut self) {
        unsafe {
            Vec::from_raw_parts(
                self.decorations as *mut SpanProbeDecoration,
                self.decorations_count,
                self.decorations_count,
            )
        };
    }
}

#[repr(C)]
pub enum ProbeType<'a> {
    Metric(MetricProbe<'a>),
    Log(LogProbe<'a>),
    Span,
    SpanDecoration(SpanDecorationProbe<'a>),
}

impl<'a> From<&'a datadog_live_debugger::ProbeType> for ProbeType<'a> {
    fn from(from: &'a datadog_live_debugger::ProbeType) -> Self {
        match from {
            datadog_live_debugger::ProbeType::Metric(metric) => ProbeType::Metric(metric.into()),
            datadog_live_debugger::ProbeType::Log(log) => ProbeType::Log(log.into()),
            datadog_live_debugger::ProbeType::Span(_) => ProbeType::Span,
            datadog_live_debugger::ProbeType::SpanDecoration(span_decoration) => {
                ProbeType::SpanDecoration(span_decoration.into())
            }
        }
    }
}

#[repr(C)]
pub struct ProbeTarget<'a> {
    pub type_name: Option<CharSlice<'a>>,
    pub method_name: Option<CharSlice<'a>>,
    pub source_file: Option<CharSlice<'a>>,
    pub signature: Option<CharSlice<'a>>,
    pub lines: CharSliceVec<'a>,
    pub in_body_location: InBodyLocation,
}

impl<'a> From<&'a datadog_live_debugger::ProbeTarget> for ProbeTarget<'a> {
    fn from(from: &'a datadog_live_debugger::ProbeTarget) -> Self {
        ProbeTarget {
            type_name: from.type_name.as_ref().map(|s| s.as_str().into()).into(),
            method_name: from.method_name.as_ref().map(|s| s.as_str().into()).into(),
            source_file: from.source_file.as_ref().map(|s| s.as_str().into()).into(),
            signature: from.signature.as_ref().map(|s| s.as_str().into()).into(),
            lines: (&from.lines).into(),
            in_body_location: from.in_body_location,
        }
    }
}

#[repr(C)]
pub struct Probe<'a> {
    pub id: CharSlice<'a>,
    pub version: u64,
    pub language: Option<CharSlice<'a>>,
    pub tags: CharSliceVec<'a>,
    pub target: ProbeTarget<'a>, // "where" is rust keyword
    pub evaluate_at: EvaluateAt,
    pub probe: ProbeType<'a>,
}

impl<'a> From<&'a datadog_live_debugger::Probe> for Probe<'a> {
    fn from(from: &'a datadog_live_debugger::Probe) -> Self {
        Probe {
            id: from.id.as_str().into(),
            version: from.version,
            language: from.language.as_ref().map(|s| s.as_str().into()).into(),
            tags: (&from.tags).into(),
            target: (&from.target).into(),
            evaluate_at: from.evaluate_at,
            probe: (&from.probe).into(),
        }
    }
}

#[repr(C)]
pub struct FilterList<'a> {
    pub package_prefixes: CharSliceVec<'a>,
    pub classes: CharSliceVec<'a>,
}

impl<'a> From<&'a datadog_live_debugger::FilterList> for FilterList<'a> {
    fn from(from: &'a datadog_live_debugger::FilterList) -> Self {
        FilterList {
            package_prefixes: (&from.package_prefixes).into(),
            classes: (&from.classes).into(),
        }
    }
}

#[repr(C)]
pub struct ServiceConfiguration<'a> {
    pub id: CharSlice<'a>,
    pub allow: FilterList<'a>,
    pub deny: FilterList<'a>,
    pub sampling_snapshots_per_second: u32,
}

impl<'a> From<&'a datadog_live_debugger::ServiceConfiguration> for ServiceConfiguration<'a> {
    fn from(from: &'a datadog_live_debugger::ServiceConfiguration) -> Self {
        ServiceConfiguration {
            id: from.id.as_str().into(),
            allow: (&from.allow).into(),
            deny: (&from.deny).into(),
            sampling_snapshots_per_second: from.sampling_snapshots_per_second,
        }
    }
}

#[repr(C)]
pub enum LiveDebuggingData<'a> {
    None,
    Probe(Probe<'a>),
    ServiceConfiguration(ServiceConfiguration<'a>),
}

impl<'a> From<&'a datadog_live_debugger::LiveDebuggingData> for LiveDebuggingData<'a> {
    fn from(from: &'a datadog_live_debugger::LiveDebuggingData) -> Self {
        match from {
            datadog_live_debugger::LiveDebuggingData::Probe(probe) => {
                LiveDebuggingData::Probe(probe.into())
            }
            datadog_live_debugger::LiveDebuggingData::ServiceConfiguration(config) => {
                LiveDebuggingData::ServiceConfiguration(config.into())
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn ddog_capture_defaults() -> Capture {
    Capture::default()
}
