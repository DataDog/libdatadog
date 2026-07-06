// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use heapless::String as HeaplessString;
use serde::Serialize;

use super::fmt::hex_u32;
use super::fmt::write_i32;
use super::report::{
    CrashContext, Frame, Metadata, ProcInfo, Report, Tag, Tags, MESSAGE_CAPACITY,
    SECTION_BUF_CAPACITY,
};
use super::{capabilities, config, protocol, state};

pub trait Sink {
    fn put(&mut self, bytes: &[u8]) -> bool;
}

pub struct SliceSink<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> SliceSink<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0 }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Sink for SliceSink<'_> {
    fn put(&mut self, bytes: &[u8]) -> bool {
        let Some(end) = self.len.checked_add(bytes.len()) else {
            return false;
        };
        if end > self.buf.len() {
            return false;
        }
        self.buf[self.len..end].copy_from_slice(bytes);
        self.len = end;
        true
    }
}

pub fn push_tag(tags: &mut Tags, key: &str, value: &str) -> bool {
    if value.is_empty() {
        return true;
    }

    let mut tag = super::report::Tag::new();
    tag.push_str(key).is_ok()
        && tag.push(':').is_ok()
        && tag.push_str(value).is_ok()
        && tags.push(tag).is_ok()
}

pub fn emit_report(sink: &mut impl Sink, report: &Report<'_>, context: &CrashContext<'_>) -> bool {
    if !emit_config(sink, report.config_json)
        || !emit_metadata(sink, report)
        || !emit_additional_tags(
            sink,
            report.stage_name,
            report.stackwalk_method,
            report.capability_bits,
            report.degradation_bits,
        )
        || !emit_kind(sink)
        || !emit_json_section(
            sink,
            protocol::DD_CRASHTRACK_BEGIN_SIGINFO,
            &context.signal,
            protocol::DD_CRASHTRACK_END_SIGINFO,
        )
        || !emit_json_section(
            sink,
            protocol::DD_CRASHTRACK_BEGIN_PROCINFO,
            &ProcInfo {
                pid: context.pid,
                tid: context.tid,
            },
            protocol::DD_CRASHTRACK_END_PROCINFO,
        )
        || !emit_stacktrace(sink, context.frames)
    {
        return emit_truncated_tail(sink, report, context);
    }

    emit_message(sink, report.stage_name, &context.signal) && emit_done(sink)
}

pub fn emit_report_with_metadata(
    sink: &mut impl Sink,
    config_json: &str,
    metadata: &Metadata<'_>,
    stage_name: &str,
    context: &CrashContext<'_>,
) -> bool {
    if !emit_config(sink, config_json)
        || !emit_json_section(
            sink,
            protocol::DD_CRASHTRACK_BEGIN_METADATA,
            metadata,
            protocol::DD_CRASHTRACK_END_METADATA,
        )
        || !emit_additional_tags(sink, stage_name, "fp_pvr", 0, 0)
        || !emit_kind(sink)
        || !emit_json_section(
            sink,
            protocol::DD_CRASHTRACK_BEGIN_SIGINFO,
            &context.signal,
            protocol::DD_CRASHTRACK_END_SIGINFO,
        )
        || !emit_json_section(
            sink,
            protocol::DD_CRASHTRACK_BEGIN_PROCINFO,
            &ProcInfo {
                pid: context.pid,
                tid: context.tid,
            },
            protocol::DD_CRASHTRACK_END_PROCINFO,
        )
        || !emit_stacktrace(sink, context.frames)
    {
        capabilities::note_degraded(capabilities::DEGRADED_TRUNCATED);
        let _ = emit_additional_tags(
            sink,
            stage_name,
            "fp_pvr",
            0,
            capabilities::DEGRADED_TRUNCATED,
        );
        return emit_message(sink, stage_name, &context.signal) && emit_done(sink);
    }

    emit_message(sink, stage_name, &context.signal) && emit_done(sink)
}

pub fn emit_minimal_report(
    sink: &mut impl Sink,
    config_json: &str,
    metadata: &Metadata<'_>,
    signal: &super::report::SignalInfo,
) -> bool {
    emit_config(sink, config_json)
        && emit_json_section(
            sink,
            protocol::DD_CRASHTRACK_BEGIN_METADATA,
            metadata,
            protocol::DD_CRASHTRACK_END_METADATA,
        )
        && emit_json_section(
            sink,
            protocol::DD_CRASHTRACK_BEGIN_SIGINFO,
            signal,
            protocol::DD_CRASHTRACK_END_SIGINFO,
        )
        && emit_done(sink)
}

pub fn emit_json_section<T: Serialize>(
    sink: &mut impl Sink,
    begin: &str,
    value: &T,
    end: &str,
) -> bool {
    let mut buf = [0u8; SECTION_BUF_CAPACITY];
    match serde_json_core::to_slice(value, &mut buf) {
        Ok(len) => {
            put_marker_line(sink, begin)
                && sink.put(&buf[..len])
                && sink.put(b"\n")
                && put_marker_line(sink, end)
        }
        Err(_) => false,
    }
}

fn emit_config(sink: &mut impl Sink, config_json: &str) -> bool {
    put_marker_line(sink, protocol::DD_CRASHTRACK_BEGIN_CONFIG)
        && sink.put(config_json.as_bytes())
        && (config_json.ends_with('\n') || sink.put(b"\n"))
        && put_marker_line(sink, protocol::DD_CRASHTRACK_END_CONFIG)
}

fn emit_metadata(sink: &mut impl Sink, report: &Report<'_>) -> bool {
    let service = if report.service.is_empty() {
        report.default_service
    } else {
        report.service
    };

    let mut metadata = Metadata::new(report.library_name, report.library_version, report.family);
    push_tag(&mut metadata.tags, "language", "native")
        && push_tag(&mut metadata.tags, "runtime", "native")
        && push_tag(&mut metadata.tags, "is_crash", "true")
        && push_tag(&mut metadata.tags, "severity", "crash")
        && push_tag(&mut metadata.tags, "service", service)
        && push_tag(&mut metadata.tags, "env", report.env)
        && push_tag(&mut metadata.tags, "version", report.app_version)
        && push_tag(&mut metadata.tags, "runtime_id", report.runtime_id)
        && push_tag(
            &mut metadata.tags,
            "runtime_version",
            report.library_version,
        )
        && push_tag(
            &mut metadata.tags,
            "library_version",
            report.library_version,
        )
        && push_tag(&mut metadata.tags, "platform", report.platform)
        && push_tag(
            &mut metadata.tags,
            "injector_version",
            report.library_version,
        )
        && emit_json_section(
            sink,
            protocol::DD_CRASHTRACK_BEGIN_METADATA,
            &metadata,
            protocol::DD_CRASHTRACK_END_METADATA,
        )
}

fn emit_additional_tags(
    sink: &mut impl Sink,
    stage: &str,
    stackwalk_method: &str,
    capability_bits: u32,
    degradation_bits: u32,
) -> bool {
    let mut tags = Tags::new();
    if !push_tag(&mut tags, "stage", stage) {
        return false;
    }
    if !push_tag(&mut tags, "stackwalk_method", stackwalk_method) {
        return false;
    }
    let capabilities = hex_u32(capability_bits);
    if !push_tag(&mut tags, "capabilities", capabilities.as_str()) {
        return false;
    }
    let degradations = hex_u32(degradation_bits);
    if !push_tag(&mut tags, "degradations", degradations.as_str()) {
        return false;
    }
    for &(bit, reason) in capabilities::DEGRADATION_REASONS {
        if degradation_bits & bit != 0 && !push_tag(&mut tags, "report_degraded", reason) {
            return false;
        }
    }
    if degradation_bits & capabilities::DEGRADED_APP_HANDLER_PRESENT != 0
        && !push_app_handler_present_tags(&mut tags)
    {
        return false;
    }
    emit_json_section(
        sink,
        protocol::DD_CRASHTRACK_BEGIN_ADDITIONAL_TAGS,
        &tags,
        protocol::DD_CRASHTRACK_END_ADDITIONAL_TAGS,
    )
}

fn push_app_handler_present_tags(tags: &mut Tags) -> bool {
    for &sig in &config::CRASH_SIGNALS {
        if !state::app_handler_present(sig) {
            continue;
        }
        let mut value = Tag::new();
        if value.push_str("app_handler_present:").is_err() {
            return false;
        }
        let mut buf = [0u8; 12];
        let written = write_i32(sig, &mut buf);
        let Ok(sig) = core::str::from_utf8(&buf[..written]) else {
            return false;
        };
        if value.push_str(sig).is_err() {
            return false;
        }
        if !push_tag(tags, "report_degraded", value.as_str()) {
            return false;
        }
    }
    true
}

fn emit_kind(sink: &mut impl Sink) -> bool {
    put_marker_line(sink, protocol::DD_CRASHTRACK_BEGIN_KIND)
        && sink.put(b"\"UnixSignal\"\n")
        && put_marker_line(sink, protocol::DD_CRASHTRACK_END_KIND)
}

fn emit_stacktrace(sink: &mut impl Sink, frames: &[usize]) -> bool {
    if !put_marker_line(sink, protocol::DD_CRASHTRACK_BEGIN_STACKTRACE) {
        return false;
    }

    let mut buf = [0u8; SECTION_BUF_CAPACITY];
    for ip in frames {
        if *ip == 0 {
            continue;
        }

        let frame = Frame::from_ip(*ip);
        let Ok(len) = serde_json_core::to_slice(&frame, &mut buf) else {
            return false;
        };
        if !(sink.put(&buf[..len]) && sink.put(b"\n")) {
            return false;
        }
    }

    put_marker_line(sink, protocol::DD_CRASHTRACK_END_STACKTRACE)
}

fn emit_message(
    sink: &mut impl Sink,
    stage_name: &str,
    signal: &super::report::SignalInfo,
) -> bool {
    let mut message = HeaplessString::<MESSAGE_CAPACITY>::new();
    message.push_str("Crash during ").is_ok()
        && message.push_str(stage_name).is_ok()
        && message.push_str(" (").is_ok()
        && message.push_str(signal.si_signo_human_readable).is_ok()
        && message.push(')').is_ok()
        && put_marker_line(sink, protocol::DD_CRASHTRACK_BEGIN_MESSAGE)
        && sink.put(message.as_bytes())
        && sink.put(b"\n")
        && put_marker_line(sink, protocol::DD_CRASHTRACK_END_MESSAGE)
}

fn emit_truncated_tail(
    sink: &mut impl Sink,
    report: &Report<'_>,
    context: &CrashContext<'_>,
) -> bool {
    capabilities::note_degraded(capabilities::DEGRADED_TRUNCATED);
    let _ = emit_additional_tags(
        sink,
        report.stage_name,
        report.stackwalk_method,
        report.capability_bits,
        report.degradation_bits | capabilities::DEGRADED_TRUNCATED,
    );
    emit_message(sink, report.stage_name, &context.signal) && emit_done(sink)
}

fn put_marker_line(sink: &mut impl Sink, marker: &str) -> bool {
    sink.put(marker.as_bytes()) && sink.put(b"\n")
}

fn emit_done(sink: &mut impl Sink) -> bool {
    put_marker_line(sink, protocol::DD_CRASHTRACK_DONE)
}
