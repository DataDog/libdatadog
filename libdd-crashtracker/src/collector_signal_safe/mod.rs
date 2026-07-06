// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Signal-safe Unix crash collection.
//!
//! `init` takes explicit caller-provided configuration and does not read environment variables.
//! `init_from_env` is the preload/bootstrap compatibility entry point and is the only path that
//! reads `DD_CRASHTRACKING_*`, `DD_SERVICE`, `DD_ENV`, `DD_VERSION`, and `DD_RUNTIME_ID`.
//!
//! Support matrix:
//!
//! | Target | fork collection | stackwalk | fallback |
//! | --- | --- | --- | --- |
//! | Linux x86_64/aarch64 | raw `clone(SIGCHLD)` | frame-pointer walk + `process_vm_readv` | `report_fd` |
//! | other Linux arches | no | no | `report_fd` |
//! | macOS/iOS | no | no | `report_fd` with siginfo-only minimal reports |
//! | non-Unix | unsupported | unsupported | compile error |
//!
//! `create_alt_stack` installs the built-in alternate signal stack only for the init thread.
//! `use_alt_stack` may be used with a caller-installed per-thread alternate stack. Stack-overflow
//! crashes on threads without an alternate stack are collected on the faulting thread's stack.
//! When `block_signals` is enabled, app handlers invoked from this handler run with the
//! crash-signal mask in effect; a nested crash on another managed signal is deferred until the
//! app handler returns.

mod backtrace;
pub(crate) mod capabilities;
mod config;
mod emitter;
mod fmt;
mod handler;
mod policy;
mod report;
mod state;
mod sys;

use crate::shared::signal_names;

#[cfg(test)]
pub(crate) static TEST_GLOBAL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub use config::SignalSafeInitConfig;
#[cfg(test)]
pub(crate) use emitter::SliceSink;
pub(crate) use emitter::{emit_report, Sink};
#[cfg(test)]
pub(crate) use fmt::hex_addr;
pub use handler::{bootstrap_complete, init_from_env_result, init_result, shutdown, InitResult};
pub(crate) use report::{CrashContext, Report, SignalInfo, SECTION_BUF_CAPACITY};
#[cfg(test)]
pub(crate) use signal_names::*;
pub use state::{set_stage, Stage};
#[doc(hidden)]
pub use sys::cstr_bytes_bounded;

pub fn capability_bits() -> u32 {
    capabilities::get().bits()
}

pub fn degradation_bits() -> u32 {
    capabilities::degradations().bits()
}

pub fn owned_signal_count() -> u32 {
    state::owned_signal_count()
}

pub fn owns_signal(sig: i32) -> bool {
    state::owns_signal(sig)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::ToOwned;
    use std::str;
    use std::string::String;

    #[test]
    fn slice_sink_reports_capacity_failure() {
        let mut buf = [0u8; 3];
        let mut sink = SliceSink::new(&mut buf);

        assert!(sink.put(b"abc"));
        assert!(!sink.put(b"d"));
        assert_eq!(sink.as_slice(), b"abc");
    }

    #[test]
    fn oversized_metadata_still_terminates_report_with_degradation() {
        let signal = SignalInfo::new(libc::SIGSEGV, SEGV_ACCERR, 0x4321, true);
        let frames = [0x10usize];
        let context = CrashContext {
            signal,
            pid: 123,
            tid: 456,
            frames: &frames,
        };
        let oversized_library_name = "x".repeat(SECTION_BUF_CAPACITY);
        let report = Report {
            config_json: "{\"resolve_frames\":\"Disabled\"}",
            library_name: &oversized_library_name,
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
            capabilities: capabilities::Capabilities::from_bits(0x21),
            degradations: capabilities::Degradations::empty(),
        };

        let mut buf = [0u8; 4096];
        let mut sink = SliceSink::new(&mut buf);
        assert!(emit_report(&mut sink, &report, &context));

        let report = str::from_utf8(sink.as_slice()).unwrap();
        assert!(report.contains("\"report_degraded:truncated\""));
        assert!(report.contains("DD_CRASHTRACK_BEGIN_MESSAGE\n"));
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
            capabilities: capabilities::Capabilities::from_bits(0x21),
            degradations: capabilities::Degradations::empty(),
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

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn emitted_wire_matches_golden_fixture() {
        let emitted = golden_report();
        assert_eq!(
            emitted,
            include_str!("../../tests/fixtures/signal_safe_report.golden")
        );
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    #[ignore = "regenerates the signal-safe emitted-wire golden fixture"]
    fn regenerate_signal_safe_report_golden() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/signal_safe_report.golden");
        std::fs::write(path, golden_report()).expect("write signal-safe golden fixture");
    }

    #[cfg(target_pointer_width = "64")]
    fn golden_report() -> String {
        let signal = SignalInfo::new(libc::SIGSEGV, SEGV_MAPERR, 0x1234, true);
        let frames = [0x10usize, 0x20usize];
        let context = CrashContext {
            signal,
            pid: 123,
            tid: 456,
            frames: &frames,
        };
        let report = Report {
            config_json: "{\"resolve_frames\":\"Disabled\"}",
            library_name: "dd-test",
            library_version: "1.2.3",
            family: "native",
            default_service: "default-service",
            service: "svc",
            env: "prod",
            app_version: "v1",
            runtime_id: "rid",
            platform: "linux",
            stage_name: "application",
            stackwalk_method: "fp_pvr",
            capabilities: capabilities::Capabilities::from_bits(0x21),
            degradations: capabilities::DEGRADED_REPORT_TO_FD,
        };

        let mut buf = [0u8; 8192];
        let mut sink = SliceSink::new(&mut buf);
        assert!(emit_report(&mut sink, &report, &context));
        str::from_utf8(sink.as_slice()).unwrap().to_owned()
    }

    fn section<'a>(report: &'a str, begin: &str, end: &str) -> &'a str {
        let start = report.find(begin).unwrap() + begin.len();
        let remaining = &report[start..];
        let finish = remaining.find(end).unwrap();
        &remaining[..finish]
    }
}
