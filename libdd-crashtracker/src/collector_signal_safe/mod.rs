// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::ffi::c_void;
use core::fmt::Write;

use heapless::{String as HeaplessString, Vec as HeaplessVec};
use serde::Serialize;

mod backtrace;
mod capabilities;
mod config;
mod handler;
mod state;
mod sys;

#[cfg(test)]
pub(crate) static TEST_GLOBAL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub use config::{build_config_json, prepare, prepare_from_env, SignalSafeInitConfig};
pub use handler::{
    bootstrap_complete, init, init_from_env, init_from_env_result, init_result, shutdown,
    InitResult,
};
pub use state::{set_stage, Stage};
pub use sys::FdSink;

pub const SECTION_BUF_CAPACITY: usize = 4096;
pub const TAG_CAPACITY: usize = 288;
pub const MAX_TAGS: usize = 20;
pub const FRAME_IP_CAPACITY: usize = 2 + core::mem::size_of::<usize>() * 2;
pub const MESSAGE_CAPACITY: usize = 192;

pub type Tag = HeaplessString<TAG_CAPACITY>;
pub type Tags = HeaplessVec<Tag, MAX_TAGS>;

pub fn capability_bits() -> u32 {
    capabilities::get()
}

pub fn degradation_bits() -> u32 {
    capabilities::degradations()
}

pub fn owned_signal_count() -> u32 {
    state::owned_signal_count()
}

pub fn owns_signal(sig: i32) -> bool {
    state::owns_signal(sig)
}

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Disposition {
    Default,
    Ignore,
    Handler,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChainAction {
    InvokeApp,
    RestoreDefaultAndRefault,
    RestoreDefaultAndReraise,
    Resume,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalContext {
    pub has_siginfo: bool,
    pub si_code: i32,
    pub si_pid: i32,
    pub self_pid: i32,
}

impl SignalContext {
    pub fn is_genuine_fault(self) -> bool {
        is_genuine_fault(self.has_siginfo, self.si_code, self.si_pid, self.self_pid)
    }
}

pub fn disposition_of(handler: *mut c_void) -> Disposition {
    match handler as usize {
        SIG_DFL_VALUE => Disposition::Default,
        SIG_IGN_VALUE => Disposition::Ignore,
        _ => Disposition::Handler,
    }
}

pub fn app_handler_is_real(handler: *mut c_void) -> bool {
    matches!(disposition_of(handler), Disposition::Handler)
}

pub fn should_run_app_first(force_on_top: bool, app_is_real: bool) -> bool {
    !force_on_top && app_is_real
}

pub fn app_recovered(handler_after: *mut c_void) -> bool {
    disposition_of(handler_after) != Disposition::Default
}

pub fn is_genuine_fault(has_siginfo: bool, si_code: i32, si_pid: i32, self_pid: i32) -> bool {
    if !has_siginfo {
        return false;
    }
    if si_code != SI_USER && si_code != SI_TKILL {
        return true;
    }
    si_pid == self_pid
}

pub fn chain_action(disposition: Disposition, has_siginfo: bool, si_code: i32) -> ChainAction {
    match disposition {
        Disposition::Ignore => ChainAction::Resume,
        Disposition::Handler => ChainAction::InvokeApp,
        Disposition::Default if has_siginfo && si_code > 0 => ChainAction::RestoreDefaultAndRefault,
        Disposition::Default => ChainAction::RestoreDefaultAndReraise,
    }
}

#[derive(Serialize)]
pub struct Metadata<'a> {
    pub library_name: &'a str,
    pub library_version: &'a str,
    pub family: &'a str,
    pub tags: Tags,
}

impl<'a> Metadata<'a> {
    pub fn new(library_name: &'a str, library_version: &'a str, family: &'a str) -> Self {
        Self {
            library_name,
            library_version,
            family,
            tags: Tags::new(),
        }
    }
}

#[derive(Serialize)]
pub struct SignalInfo {
    pub si_signo: i32,
    pub si_code: i32,
    pub si_signo_human_readable: &'static str,
    pub si_code_human_readable: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub si_addr: Option<HeaplessString<FRAME_IP_CAPACITY>>,
}

impl SignalInfo {
    pub fn new(si_signo: i32, si_code: i32, si_addr: usize, has_siginfo: bool) -> Self {
        let si_addr = if has_siginfo && signal_has_address(si_signo) {
            Some(hex_addr(si_addr))
        } else {
            None
        };

        Self {
            si_signo,
            si_code,
            si_signo_human_readable: rust_signal_name(si_signo),
            si_code_human_readable: rust_si_code_name(si_signo, si_code),
            si_addr,
        }
    }
}

#[derive(Serialize)]
pub struct ProcInfo {
    pub pid: i32,
    pub tid: i32,
}

#[derive(Serialize)]
pub struct Frame {
    pub ip: HeaplessString<FRAME_IP_CAPACITY>,
}

impl Frame {
    pub fn from_ip(ip: usize) -> Self {
        Self { ip: hex_addr(ip) }
    }
}

pub struct CrashContext<'a> {
    pub signal: SignalInfo,
    pub pid: i32,
    pub tid: i32,
    pub frames: &'a [usize],
}

pub struct Report<'a> {
    pub config_json: &'a str,
    pub library_name: &'a str,
    pub library_version: &'a str,
    pub family: &'a str,
    pub default_service: &'a str,
    pub service: &'a str,
    pub env: &'a str,
    pub app_version: &'a str,
    pub runtime_id: &'a str,
    pub platform: &'a str,
    pub stage_name: &'a str,
    pub stackwalk_method: &'a str,
    pub capability_bits: u32,
    pub degradation_bits: u32,
}

pub fn push_tag(tags: &mut Tags, key: &str, value: &str) -> bool {
    if value.is_empty() {
        return true;
    }

    let mut tag = Tag::new();
    tag.push_str(key).is_ok()
        && tag.push(':').is_ok()
        && tag.push_str(value).is_ok()
        && tags.push(tag).is_ok()
}

pub fn emit_report(sink: &mut impl Sink, report: &Report<'_>, context: &CrashContext<'_>) -> bool {
    emit_config(sink, report.config_json)
        && emit_metadata(sink, report)
        && emit_additional_tags(
            sink,
            report.stage_name,
            report.stackwalk_method,
            report.capability_bits,
            report.degradation_bits,
        )
        && emit_kind(sink)
        && emit_json_section(
            sink,
            b"DD_CRASHTRACK_BEGIN_SIGINFO\n",
            &context.signal,
            b"DD_CRASHTRACK_END_SIGINFO\n",
        )
        && emit_json_section(
            sink,
            b"DD_CRASHTRACK_BEGIN_PROCESSINFO\n",
            &ProcInfo {
                pid: context.pid,
                tid: context.tid,
            },
            b"DD_CRASHTRACK_END_PROCESSINFO\n",
        )
        && emit_stacktrace(sink, context.frames)
        && emit_message(sink, report.stage_name, &context.signal)
        && sink.put(b"DD_CRASHTRACK_DONE\n")
}

pub fn emit_report_with_metadata(
    sink: &mut impl Sink,
    config_json: &str,
    metadata: &Metadata<'_>,
    stage_name: &str,
    context: &CrashContext<'_>,
) -> bool {
    emit_config(sink, config_json)
        && emit_json_section(
            sink,
            b"DD_CRASHTRACK_BEGIN_METADATA\n",
            metadata,
            b"DD_CRASHTRACK_END_METADATA\n",
        )
        && emit_additional_tags(sink, stage_name, "fp_pvr", 0, 0)
        && emit_kind(sink)
        && emit_json_section(
            sink,
            b"DD_CRASHTRACK_BEGIN_SIGINFO\n",
            &context.signal,
            b"DD_CRASHTRACK_END_SIGINFO\n",
        )
        && emit_json_section(
            sink,
            b"DD_CRASHTRACK_BEGIN_PROCESSINFO\n",
            &ProcInfo {
                pid: context.pid,
                tid: context.tid,
            },
            b"DD_CRASHTRACK_END_PROCESSINFO\n",
        )
        && emit_stacktrace(sink, context.frames)
        && emit_message(sink, stage_name, &context.signal)
        && sink.put(b"DD_CRASHTRACK_DONE\n")
}

pub fn emit_minimal_report(
    sink: &mut impl Sink,
    config_json: &str,
    metadata: &Metadata<'_>,
    signal: &SignalInfo,
) -> bool {
    emit_config(sink, config_json)
        && emit_json_section(
            sink,
            b"DD_CRASHTRACK_BEGIN_METADATA\n",
            metadata,
            b"DD_CRASHTRACK_END_METADATA\n",
        )
        && emit_json_section(
            sink,
            b"DD_CRASHTRACK_BEGIN_SIGINFO\n",
            signal,
            b"DD_CRASHTRACK_END_SIGINFO\n",
        )
        && sink.put(b"DD_CRASHTRACK_DONE\n")
}

pub fn emit_json_section<T: Serialize>(
    sink: &mut impl Sink,
    begin: &[u8],
    value: &T,
    end: &[u8],
) -> bool {
    let mut buf = [0u8; SECTION_BUF_CAPACITY];
    match serde_json_core::to_slice(value, &mut buf) {
        Ok(len) => sink.put(begin) && sink.put(&buf[..len]) && sink.put(b"\n") && sink.put(end),
        Err(_) => false,
    }
}

pub fn rust_signal_name(signal: i32) -> &'static str {
    match signal {
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGILL => "SIGILL",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGSYS => "SIGSYS",
        libc::SIGTRAP => "SIGTRAP",
        _ => "<unknown>",
    }
}

pub fn rust_si_code_name(signal: i32, si_code: i32) -> &'static str {
    match si_code {
        SI_USER => "SI_USER",
        SI_KERNEL => "SI_KERNEL",
        SI_QUEUE => "SI_QUEUE",
        SI_TIMER => "SI_TIMER",
        SI_MESGQ => "SI_MESGQ",
        SI_ASYNCIO => "SI_ASYNCIO",
        SI_SIGIO => "SI_SIGIO",
        SI_TKILL => "SI_TKILL",
        _ => signal_specific_si_code_name(signal, si_code),
    }
}

pub fn signal_has_address(signal: i32) -> bool {
    matches!(
        signal,
        libc::SIGBUS | libc::SIGFPE | libc::SIGILL | libc::SIGSEGV | libc::SIGTRAP
    )
}

pub fn hex_addr(value: usize) -> HeaplessString<FRAME_IP_CAPACITY> {
    let mut out = HeaplessString::new();
    let _ = out.push_str("0x");

    for shift in (0..core::mem::size_of::<usize>() * 2).rev() {
        let nibble = ((value >> (shift * 4)) & 0xf) as u8;
        let ch = if nibble < 10 {
            b'0' + nibble
        } else {
            b'a' + (nibble - 10)
        };
        let _ = out.push(ch as char);
    }

    out
}

fn emit_config(sink: &mut impl Sink, config_json: &str) -> bool {
    sink.put(b"DD_CRASHTRACK_BEGIN_CONFIG\n")
        && sink.put(config_json.as_bytes())
        && (config_json.ends_with('\n') || sink.put(b"\n"))
        && sink.put(b"DD_CRASHTRACK_END_CONFIG\n")
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
            b"DD_CRASHTRACK_BEGIN_METADATA\n",
            &metadata,
            b"DD_CRASHTRACK_END_METADATA\n",
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
    emit_json_section(
        sink,
        b"DD_CRASHTRACK_BEGIN_ADDITIONAL_TAGS\n",
        &tags,
        b"DD_CRASHTRACK_END_ADDITIONAL_TAGS\n",
    )
}

fn emit_kind(sink: &mut impl Sink) -> bool {
    sink.put(b"DD_CRASHTRACK_BEGIN_KIND\n\"UnixSignal\"\nDD_CRASHTRACK_END_KIND\n")
}

fn hex_u32(value: u32) -> HeaplessString<10> {
    let mut out = HeaplessString::new();
    let _ = write!(out, "0x{value:08x}");
    out
}

fn emit_stacktrace(sink: &mut impl Sink, frames: &[usize]) -> bool {
    if !sink.put(b"DD_CRASHTRACK_BEGIN_STACKTRACE\n") {
        return false;
    }

    for ip in frames {
        if *ip == 0 {
            continue;
        }

        let frame = Frame::from_ip(*ip);
        let mut buf = [0u8; SECTION_BUF_CAPACITY];
        let Ok(len) = serde_json_core::to_slice(&frame, &mut buf) else {
            return false;
        };
        if !(sink.put(&buf[..len]) && sink.put(b"\n")) {
            return false;
        }
    }

    sink.put(b"DD_CRASHTRACK_END_STACKTRACE\n")
}

fn emit_message(sink: &mut impl Sink, stage_name: &str, signal: &SignalInfo) -> bool {
    let mut message = HeaplessString::<MESSAGE_CAPACITY>::new();
    message.push_str("Crash during ").is_ok()
        && message.push_str(stage_name).is_ok()
        && message.push_str(" (").is_ok()
        && message.push_str(signal.si_signo_human_readable).is_ok()
        && message.push(')').is_ok()
        && sink.put(b"DD_CRASHTRACK_BEGIN_MESSAGE\n")
        && sink.put(message.as_bytes())
        && sink.put(b"\nDD_CRASHTRACK_END_MESSAGE\n")
}

fn signal_specific_si_code_name(signal: i32, si_code: i32) -> &'static str {
    match signal {
        libc::SIGSEGV => match si_code {
            SEGV_MAPERR => "SEGV_MAPERR",
            SEGV_ACCERR => "SEGV_ACCERR",
            _ => "<unknown>",
        },
        libc::SIGBUS => match si_code {
            BUS_ADRALN => "BUS_ADRALN",
            BUS_ADRERR => "BUS_ADRERR",
            BUS_OBJERR => "BUS_OBJERR",
            _ => "<unknown>",
        },
        libc::SIGILL => match si_code {
            ILL_ILLOPC => "ILL_ILLOPC",
            ILL_ILLOPN => "ILL_ILLOPN",
            ILL_ILLADR => "ILL_ILLADR",
            ILL_ILLTRP => "ILL_ILLTRP",
            ILL_PRVOPC => "ILL_PRVOPC",
            ILL_PRVREG => "ILL_PRVREG",
            ILL_COPROC => "ILL_COPROC",
            ILL_BADSTK => "ILL_BADSTK",
            _ => "<unknown>",
        },
        _ => "<unknown>",
    }
}

const SIG_DFL_VALUE: usize = 0;
const SIG_IGN_VALUE: usize = 1;

pub const SI_USER: i32 = 0;
pub const SI_KERNEL: i32 = 128;
pub const SI_QUEUE: i32 = -1;
pub const SI_TIMER: i32 = -2;
pub const SI_MESGQ: i32 = -3;
pub const SI_ASYNCIO: i32 = -4;
pub const SI_SIGIO: i32 = -5;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub const SI_TKILL: i32 = -6;

#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const SI_TKILL: i32 = i32::MIN;

pub const SEGV_MAPERR: i32 = 1;
pub const SEGV_ACCERR: i32 = 2;

pub const BUS_ADRALN: i32 = 1;
pub const BUS_ADRERR: i32 = 2;
pub const BUS_OBJERR: i32 = 3;

pub const ILL_ILLOPC: i32 = 1;
pub const ILL_ILLOPN: i32 = 2;
pub const ILL_ILLADR: i32 = 3;
pub const ILL_ILLTRP: i32 = 4;
pub const ILL_PRVOPC: i32 = 5;
pub const ILL_PRVREG: i32 = 6;
pub const ILL_COPROC: i32 = 7;
pub const ILL_BADSTK: i32 = 8;

#[cfg(test)]
mod tests {
    use super::*;
    use std::str;

    #[test]
    fn slice_sink_reports_capacity_failure() {
        let mut buf = [0u8; 3];
        let mut sink = SliceSink::new(&mut buf);

        assert!(sink.put(b"abc"));
        assert!(!sink.put(b"d"));
        assert_eq!(sink.as_slice(), b"abc");
    }

    #[test]
    fn dispositions_match_sigaction_sentinels() {
        let dfl = SIG_DFL_VALUE as *mut c_void;
        let ign = SIG_IGN_VALUE as *mut c_void;
        let handler = 0x1234usize as *mut c_void;

        assert_eq!(disposition_of(dfl), Disposition::Default);
        assert_eq!(disposition_of(core::ptr::null_mut()), Disposition::Default);
        assert_eq!(disposition_of(ign), Disposition::Ignore);
        assert_eq!(disposition_of(handler), Disposition::Handler);
        assert!(!app_handler_is_real(dfl));
        assert!(!app_handler_is_real(ign));
        assert!(app_handler_is_real(handler));
    }

    #[test]
    fn handler_policy_tracks_application_recovery() {
        let dfl = SIG_DFL_VALUE as *mut c_void;
        let ign = SIG_IGN_VALUE as *mut c_void;
        let handler = 0x1234usize as *mut c_void;

        assert!(should_run_app_first(false, true));
        assert!(!should_run_app_first(true, true));
        assert!(!should_run_app_first(false, false));

        assert!(app_recovered(handler));
        assert!(app_recovered(ign));
        assert!(!app_recovered(dfl));
    }

    #[test]
    fn disposition_based_chain_action_resumes_ignored_signals() {
        assert_eq!(
            chain_action(Disposition::Ignore, true, SEGV_MAPERR),
            ChainAction::Resume
        );
    }

    #[test]
    fn genuine_fault_filter_ignores_external_async_signal() {
        let ctx = SignalContext {
            has_siginfo: true,
            si_code: SI_USER,
            si_pid: 7,
            self_pid: 9,
        };

        assert!(!ctx.is_genuine_fault());
    }

    #[test]
    fn genuine_fault_filter_accepts_self_sent_async_signal() {
        let ctx = SignalContext {
            has_siginfo: true,
            si_code: SI_USER,
            si_pid: 9,
            self_pid: 9,
        };

        assert!(ctx.is_genuine_fault());
    }

    #[test]
    fn chain_action_matches_default_signal_semantics() {
        assert_eq!(
            chain_action(Disposition::Default, true, SEGV_MAPERR),
            ChainAction::RestoreDefaultAndRefault
        );
        assert_eq!(
            chain_action(Disposition::Default, true, SI_USER),
            ChainAction::RestoreDefaultAndReraise
        );
        assert_eq!(
            chain_action(Disposition::Handler, true, SEGV_MAPERR),
            ChainAction::InvokeApp
        );
    }

    #[test]
    fn signal_names_cover_common_native_faults() {
        assert_eq!(rust_signal_name(libc::SIGSEGV), "SIGSEGV");
        assert_eq!(rust_si_code_name(libc::SIGSEGV, SEGV_MAPERR), "SEGV_MAPERR");
        assert_eq!(rust_si_code_name(libc::SIGBUS, BUS_ADRALN), "BUS_ADRALN");
        assert_eq!(rust_si_code_name(libc::SIGILL, ILL_ILLOPC), "ILL_ILLOPC");
        assert_eq!(rust_si_code_name(libc::SIGSEGV, SI_USER), "SI_USER");
        assert_eq!(rust_si_code_name(libc::SIGSEGV, 999), "<unknown>");
    }

    #[test]
    fn minimal_report_emits_json_sections() {
        let mut metadata = Metadata::new("lib", "1.0.0", "native");
        assert!(push_tag(&mut metadata.tags, "stage", "application"));

        let signal = SignalInfo::new(libc::SIGSEGV, SEGV_MAPERR, 0x1234, true);

        let mut buf = [0u8; 1024];
        let mut sink = SliceSink::new(&mut buf);
        assert!(emit_minimal_report(
            &mut sink,
            "{\"resolve_frames\":\"Disabled\"}",
            &metadata,
            &signal
        ));

        let report = str::from_utf8(sink.as_slice()).unwrap();
        let metadata = section(
            report,
            "DD_CRASHTRACK_BEGIN_METADATA\n",
            "DD_CRASHTRACK_END_METADATA\n",
        );
        let signal = section(
            report,
            "DD_CRASHTRACK_BEGIN_SIGINFO\n",
            "DD_CRASHTRACK_END_SIGINFO\n",
        );

        let metadata: serde_json::Value = serde_json::from_str(metadata.trim()).unwrap();
        let signal: serde_json::Value = serde_json::from_str(signal.trim()).unwrap();

        assert_eq!(metadata["library_name"], "lib");
        assert_eq!(metadata["tags"][0], "stage:application");
        assert_eq!(signal["si_signo"], libc::SIGSEGV);
        assert_eq!(signal["si_signo_human_readable"], "SIGSEGV");
        assert_eq!(signal["si_code_human_readable"], "SEGV_MAPERR");
        assert_eq!(signal["si_addr"], hex_addr(0x1234).as_str());
        assert!(report.ends_with("DD_CRASHTRACK_DONE\n"));
    }

    #[test]
    fn full_report_emits_native_section_shape() {
        let signal = SignalInfo::new(libc::SIGSEGV, SEGV_ACCERR, 0x4321, true);
        let frames = [0x10usize, 0, 0x20usize];
        let context = CrashContext {
            signal,
            pid: 123,
            tid: 456,
            frames: &frames,
        };
        let report = Report {
            config_json: "{\"resolve_frames\":\"Disabled\"}",
            library_name: config::COMPAT_LIBRARY_NAME,
            library_version: "golden-1.0",
            family: config::COMPAT_LIBRARY_FAMILY,
            default_service: config::COMPAT_DEFAULT_SERVICE,
            service: "",
            env: "prod",
            app_version: "v1",
            runtime_id: "rid",
            platform: "linux",
            stage_name: "application",
            stackwalk_method: "fp_pvr",
            capability_bits: 0x21,
            degradation_bits: 0,
        };

        let mut buf = [0u8; 4096];
        let mut sink = SliceSink::new(&mut buf);
        assert!(emit_report(&mut sink, &report, &context));

        let report = str::from_utf8(sink.as_slice()).unwrap();
        let metadata = section(
            report,
            "DD_CRASHTRACK_BEGIN_METADATA\n",
            "DD_CRASHTRACK_END_METADATA\n",
        );
        let tags = section(
            report,
            "DD_CRASHTRACK_BEGIN_ADDITIONAL_TAGS\n",
            "DD_CRASHTRACK_END_ADDITIONAL_TAGS\n",
        );
        let kind = section(
            report,
            "DD_CRASHTRACK_BEGIN_KIND\n",
            "DD_CRASHTRACK_END_KIND\n",
        );
        let procinfo = section(
            report,
            "DD_CRASHTRACK_BEGIN_PROCESSINFO\n",
            "DD_CRASHTRACK_END_PROCESSINFO\n",
        );
        let stacktrace = section(
            report,
            "DD_CRASHTRACK_BEGIN_STACKTRACE\n",
            "DD_CRASHTRACK_END_STACKTRACE\n",
        );
        let message = section(
            report,
            "DD_CRASHTRACK_BEGIN_MESSAGE\n",
            "DD_CRASHTRACK_END_MESSAGE\n",
        );

        let metadata: serde_json::Value = serde_json::from_str(metadata.trim()).unwrap();
        let tags: serde_json::Value = serde_json::from_str(tags.trim()).unwrap();
        let kind: serde_json::Value = serde_json::from_str(kind.trim()).unwrap();
        let procinfo: serde_json::Value = serde_json::from_str(procinfo.trim()).unwrap();

        assert_eq!(metadata["library_name"], config::COMPAT_LIBRARY_NAME);
        assert_eq!(metadata["library_version"], "golden-1.0");
        assert_eq!(metadata["tags"][0], "language:native");
        assert_eq!(
            metadata["tags"][4]
                .as_str()
                .unwrap()
                .strip_prefix("service:"),
            Some(config::COMPAT_DEFAULT_SERVICE)
        );
        assert_eq!(metadata["tags"][5], "env:prod");
        assert_eq!(metadata["tags"][6], "version:v1");
        assert_eq!(tags[0], "stage:application");
        assert_eq!(tags[1], "stackwalk_method:fp_pvr");
        assert_eq!(tags[2], "capabilities:0x00000021");
        assert_eq!(tags[3], "degradations:0x00000000");
        assert_eq!(kind, "UnixSignal");
        assert_eq!(procinfo["pid"], 123);
        assert_eq!(procinfo["tid"], 456);
        assert!(stacktrace.contains(hex_addr(0x10).as_str()));
        assert!(stacktrace.contains(hex_addr(0x20).as_str()));
        assert!(!stacktrace.contains(hex_addr(0).as_str()));
        assert_eq!(message.trim(), "Crash during application (SIGSEGV)");
        assert!(report.ends_with("DD_CRASHTRACK_DONE\n"));
    }

    fn section<'a>(report: &'a str, begin: &str, end: &str) -> &'a str {
        let start = report.find(begin).unwrap() + begin.len();
        let remaining = &report[start..];
        let finish = remaining.find(end).unwrap();
        &remaining[..finish]
    }
}
