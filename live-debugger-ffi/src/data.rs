// Unless explicitly stated otherwise all files in this repository are licensed under the Apache
// License Version 2.0. This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use datadog_live_debugger::debugger_defs::{ProbeMetadata, ProbeMetadataLocation};
use datadog_live_debugger::{
    Capture, DslString, EvaluateAt, InBodyLocation, MetricKind, ProbeCondition, ProbeValue,
    SpanProbeTarget,
};
use ddcommon_ffi::slice::AsBytes;
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
    pub capture_snapshot: bool,
    pub sampling_snapshots_per_second: u32,
}

impl<'a> From<&'a datadog_live_debugger::LogProbe> for LogProbe<'a> {
    fn from(from: &'a datadog_live_debugger::LogProbe) -> Self {
        LogProbe {
            segments: &from.segments,
            when: &from.when,
            capture: &from.capture,
            capture_snapshot: from.capture_snapshot,
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
pub struct SpanProbeTag<'a> {
    pub tag: Tag<'a>,
    pub next_condition: bool,
}

#[repr(C)]
pub struct SpanDecorationProbe<'a> {
    pub target: SpanProbeTarget,
    pub conditions: *const &'a ProbeCondition,
    pub span_tags: *const SpanProbeTag<'a>,
    pub span_tags_num: usize,
}

impl<'a> From<&'a datadog_live_debugger::SpanDecorationProbe> for SpanDecorationProbe<'a> {
    fn from(from: &'a datadog_live_debugger::SpanDecorationProbe) -> Self {
        let mut tags = vec![];
        let mut conditions = vec![];
        for decoration in from.decorations.iter() {
            let mut next_condition = true;
            for (name, value) in decoration.tags.iter() {
                tags.push(SpanProbeTag {
                    tag: Tag {
                        name: CharSlice::from(name.as_str()),
                        value,
                    },
                    next_condition,
                });
                next_condition = false;
            }
            conditions.push(&decoration.condition);
        }
        let new = SpanDecorationProbe {
            target: from.target,
            conditions: conditions.as_ptr(),
            span_tags: tags.as_ptr(),
            span_tags_num: tags.len(),
        };
        std::mem::forget(tags);
        new
    }
}

#[no_mangle]
extern "C" fn drop_span_decoration_probe(_: SpanDecorationProbe) {}

impl<'a> Drop for SpanDecorationProbe<'a> {
    fn drop(&mut self) {
        unsafe {
            let tags = Vec::from_raw_parts(
                self.span_tags as *mut SpanProbeTag,
                self.span_tags_num,
                self.span_tags_num,
            );
            let num_conditions = tags.iter().filter(|p| p.next_condition).count();
            _ = Vec::from_raw_parts(
                self.conditions as *mut ProbeCondition,
                num_conditions,
                num_conditions,
            );
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

impl<'a> From<&Probe<'a>> for ProbeMetadata<'a> {
    fn from(val: &Probe<'a>) -> Self {
        // SAFETY: These values are unmodified original rust strings. Just convert it back.
        ProbeMetadata {
            id: unsafe { val.id.assume_utf8() }.into(),
            location: ProbeMetadataLocation {
                method: val
                    .target
                    .method_name
                    .to_std_ref()
                    .map(|s| unsafe { s.assume_utf8() }.into()),
                r#type: val
                    .target
                    .type_name
                    .to_std_ref()
                    .map(|s| unsafe { s.assume_utf8() }.into()),
            },
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

pub extern "C" fn ddog_capture_defaults() -> Capture {
    Capture::default()
}
