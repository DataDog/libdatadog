// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use heapless::String as HeaplessString;
use serde::Serialize;

use super::capabilities::{Capabilities, Degradations};
use super::fmt::hex_u32;
use super::fmt::{write_i32, I32_BUF_CAPACITY};
use super::report::{
    CrashContext, Frame, Metadata, ProcInfo, Report, Tag, Tags, MESSAGE_CAPACITY,
    SECTION_BUF_CAPACITY,
};
use super::{capabilities, config, state};
use crate::protocol;
use crate::shared::tag_keys;

pub trait Sink: protocol::ByteSink<Error = ()> {
    fn put(&mut self, bytes: &[u8]) -> bool {
        self.write_bytes(bytes).is_ok()
    }
}

impl<T: protocol::ByteSink<Error = ()>> Sink for T {}

#[cfg(test)]
pub struct SliceSink<'a> {
    buf: &'a mut [u8],
    len: usize,
}

#[cfg(test)]
impl<'a> SliceSink<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, len: 0 }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

#[cfg(test)]
impl protocol::ByteSink for SliceSink<'_> {
    type Error = ();

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        let Some(end) = self.len.checked_add(bytes.len()) else {
            return Err(());
        };
        if end > self.buf.len() {
            return Err(());
        }
        self.buf[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(())
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
    if !emit_report_sections(sink, report, context) {
        return emit_truncated_tail(sink, report, context);
    }

    emit_message(sink, &context.signal) && emit_done(sink)
}

fn emit_report_sections(
    sink: &mut impl Sink,
    report: &Report<'_>,
    context: &CrashContext<'_>,
) -> bool {
    if !emit_config(sink, report.config_json) || !emit_metadata(sink, report) {
        return false;
    }
    if !emit_additional_tags(
        sink,
        report.stackwalk_method,
        report.capabilities,
        report.degradations,
    ) {
        return false;
    }
    if !emit_kind(sink) {
        return false;
    }
    if !emit_json_section(
        sink,
        protocol::DD_CRASHTRACK_BEGIN_SIGINFO,
        &context.signal,
        protocol::DD_CRASHTRACK_END_SIGINFO,
    ) {
        return false;
    }
    if !emit_json_section(
        sink,
        protocol::DD_CRASHTRACK_BEGIN_PROCINFO,
        &ProcInfo {
            pid: context.pid,
            tid: context.tid,
        },
        protocol::DD_CRASHTRACK_END_PROCINFO,
    ) {
        return false;
    }
    emit_stacktrace(sink, context.frames)
}

pub fn emit_json_section<T: Serialize>(
    sink: &mut impl Sink,
    begin: &str,
    value: &T,
    end: &str,
) -> bool {
    let mut buf = [0u8; SECTION_BUF_CAPACITY];
    protocol::section::<_, ()>(sink, begin, end, |sink| {
        let len = serde_json_core::to_slice(value, &mut buf).map_err(|_| ())?;
        sink.write_bytes(&buf[..len])?;
        sink.write_bytes(b"\n")
    })
    .is_ok()
}

fn emit_config(sink: &mut impl Sink, config_json: &str) -> bool {
    protocol::section::<_, ()>(
        sink,
        protocol::DD_CRASHTRACK_BEGIN_CONFIG,
        protocol::DD_CRASHTRACK_END_CONFIG,
        |sink| {
            sink.write_bytes(config_json.as_bytes())?;
            if config_json.ends_with('\n') {
                Ok(())
            } else {
                sink.write_bytes(b"\n")
            }
        },
    )
    .is_ok()
}

fn emit_metadata(sink: &mut impl Sink, report: &Report<'_>) -> bool {
    let service = if report.service.is_empty() {
        report.default_service
    } else {
        report.service
    };

    let mut metadata = Metadata::new(report.library_name, report.library_version, report.family);
    push_tag(&mut metadata.tags, tag_keys::LANGUAGE, "native")
        && push_tag(&mut metadata.tags, tag_keys::RUNTIME, "native")
        && push_tag(&mut metadata.tags, tag_keys::IS_CRASH, "true")
        && push_tag(&mut metadata.tags, tag_keys::SEVERITY, "crash")
        && push_tag(&mut metadata.tags, tag_keys::SERVICE, service)
        && push_tag(&mut metadata.tags, tag_keys::ENV, report.env)
        && push_tag(&mut metadata.tags, tag_keys::VERSION, report.app_version)
        && push_tag(&mut metadata.tags, tag_keys::RUNTIME_ID, report.runtime_id)
        && push_tag(
            &mut metadata.tags,
            tag_keys::RUNTIME_VERSION,
            report.library_version,
        )
        && push_tag(
            &mut metadata.tags,
            tag_keys::LIBRARY_VERSION,
            report.library_version,
        )
        && push_tag(&mut metadata.tags, tag_keys::PLATFORM, report.platform)
        && push_tag(
            &mut metadata.tags,
            tag_keys::INJECTOR_VERSION,
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
    stackwalk_method: &str,
    capability_bits: Capabilities,
    degradation_bits: Degradations,
) -> bool {
    let mut tags = Tags::new();
    if !push_tag(&mut tags, tag_keys::STACKWALK_METHOD, stackwalk_method) {
        return false;
    }
    let capabilities = hex_u32(capability_bits.bits());
    if !push_tag(&mut tags, tag_keys::CAPABILITIES, capabilities.as_str()) {
        return false;
    }
    let degradations = hex_u32(degradation_bits.bits());
    if !push_tag(&mut tags, tag_keys::DEGRADATIONS, degradations.as_str()) {
        return false;
    }
    for &(bit, reason) in capabilities::DEGRADATION_REASONS {
        if degradation_bits.contains(bit) && !push_tag(&mut tags, tag_keys::REPORT_DEGRADED, reason)
        {
            return false;
        }
    }
    if degradation_bits.contains(capabilities::DEGRADED_APP_HANDLER_PRESENT)
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
        let mut buf = [0u8; I32_BUF_CAPACITY];
        let written = write_i32(sig, &mut buf);
        let Ok(sig) = core::str::from_utf8(&buf[..written]) else {
            return false;
        };
        if value.push_str(sig).is_err() {
            return false;
        }
        if !push_tag(tags, tag_keys::REPORT_DEGRADED, value.as_str()) {
            return false;
        }
    }
    true
}

fn emit_kind(sink: &mut impl Sink) -> bool {
    protocol::section::<_, ()>(
        sink,
        protocol::DD_CRASHTRACK_BEGIN_KIND,
        protocol::DD_CRASHTRACK_END_KIND,
        |sink| sink.write_bytes(b"\"UnixSignal\"\n"),
    )
    .is_ok()
}

fn emit_stacktrace(sink: &mut impl Sink, frames: &[usize]) -> bool {
    protocol::section::<_, ()>(
        sink,
        protocol::DD_CRASHTRACK_BEGIN_STACKTRACE,
        protocol::DD_CRASHTRACK_END_STACKTRACE,
        |sink| {
            let mut buf = [0u8; SECTION_BUF_CAPACITY];
            for ip in frames {
                if *ip == 0 {
                    continue;
                }

                let frame = Frame::from_ip(*ip);
                let len = serde_json_core::to_slice(&frame, &mut buf).map_err(|_| ())?;
                sink.write_bytes(&buf[..len])?;
                sink.write_bytes(b"\n")?;
            }
            Ok::<(), ()>(())
        },
    )
    .is_ok()
}

fn emit_message(sink: &mut impl Sink, signal: &super::report::SignalInfo) -> bool {
    let mut message = HeaplessString::<MESSAGE_CAPACITY>::new();
    message.push_str("Crash (").is_ok()
        && message.push_str(signal.si_signo_human_readable).is_ok()
        && message.push(')').is_ok()
        && protocol::section::<_, ()>(
            sink,
            protocol::DD_CRASHTRACK_BEGIN_MESSAGE,
            protocol::DD_CRASHTRACK_END_MESSAGE,
            |sink| {
                sink.write_bytes(message.as_bytes())?;
                sink.write_bytes(b"\n")
            },
        )
        .is_ok()
}

fn emit_truncated_tail(
    sink: &mut impl Sink,
    report: &Report<'_>,
    context: &CrashContext<'_>,
) -> bool {
    capabilities::note_degraded(capabilities::DEGRADED_TRUNCATED);
    let _ = emit_additional_tags(
        sink,
        report.stackwalk_method,
        report.capabilities,
        report.degradations.with(capabilities::DEGRADED_TRUNCATED),
    );
    emit_message(sink, &context.signal) && emit_done(sink)
}

fn emit_done(sink: &mut impl Sink) -> bool {
    protocol::marker_line::<_, ()>(sink, protocol::DD_CRASHTRACK_DONE).is_ok()
}
