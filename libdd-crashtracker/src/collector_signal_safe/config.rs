// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::sync::atomic::Ordering::Relaxed;

use heapless::String as HeaplessString;
use serde::Serialize;
use thiserror::Error;

use super::state::meta_mut;
use super::{capabilities, state, sys};
use crate::shared::{
    defaults::DD_CRASHTRACK_DEFAULT_TIMEOUT_SECS, signals::SIGNAL_SAFE_CRASH_SIGNALS,
    stacktrace_collection::StacktraceCollection,
};

// Compatibility preset for the existing C-tracer consumer. New integrators should pass
// explicit metadata through SignalSafeInitConfig instead of relying on these defaults.
pub const COMPAT_LIBRARY_VERSION: &str = match option_env!("DD_TRACE_C_VERSION") {
    Some(v) => v,
    None => "dev",
};

// Prefer the neutral build-time receiver path name. The DD_TRACE_C_* name remains as a
// lower-priority compatibility alias for existing C-tracer package builds.
const DEFAULT_RECEIVER_PATH: &str = match option_env!("DD_CRASHTRACKING_RECEIVER_PATH") {
    Some(p) => p,
    None => match option_env!("DD_TRACE_C_CRASHTRACKER_PROCESS_PATH") {
        Some(p) => p,
        None => "/opt/datadog-packages/datadog-apm-library-c/stable/process-crash-receiver",
    },
};

pub const COMPAT_LIBRARY_NAME: &str = "dd-trace-c";
pub const COMPAT_LIBRARY_FAMILY: &str = "native";
pub const COMPAT_DEFAULT_SERVICE: &str = "dd-trace-c";

/// Capacity for signal-safe filesystem path buffers (PATH_MAX + trailing NUL).
pub const PATH_CAPACITY: usize = 513;

pub const RECEIVER_TIMEOUT_SECS: u32 = DD_CRASHTRACK_DEFAULT_TIMEOUT_SECS;
pub const RECEIVER_TIMEOUT_SECS_MAX: u32 = 60;
pub const COLLECTOR_REAP_MS: i32 = 500;
pub const RECEIVER_TIMEOUT_GRACE_MS: i32 = 1000;
pub const BACKTRACE_LEVELS_DEFAULT: usize = 32;
pub const BACKTRACE_LEVELS_MAX: usize = 64;

pub const CRASH_SIGNALS: [i32; 5] = SIGNAL_SAFE_CRASH_SIGNALS;

pub const CONFIG_JSON_BUF_SIZE: usize = 2048;

#[derive(Clone, Copy, Debug)]
pub struct SignalSafeInitConfig<'a> {
    /// Receiver executable path. Empty uses the compatibility default.
    pub receiver_path: &'a [u8],
    pub service: &'a [u8],
    pub env: &'a [u8],
    pub app_version: &'a [u8],
    pub runtime_id: &'a [u8],
    pub platform: &'a [u8],
    pub library_name: &'a [u8],
    pub library_version: &'a [u8],
    pub family: &'a [u8],
    pub default_service: &'a [u8],
    pub force_on_top: bool,
    pub only_bootstrap: bool,
    pub debug_logging: bool,
    /// Install the built-in alternate signal stack on the init thread.
    ///
    /// This stack is per-thread kernel state. Other threads must install their own alternate
    /// stack before `use_alt_stack` can protect stack-overflow crashes on those threads.
    pub create_alt_stack: bool,
    /// Register crash handlers with `SA_ONSTACK`.
    ///
    /// This may be used with `create_alt_stack` or with a caller-provided alternate stack already
    /// installed on the current thread.
    pub use_alt_stack: bool,
    /// Add all managed crash signals to the handler mask.
    ///
    /// Application handlers invoked by the signal-safe handler run with this mask in effect.
    pub block_signals: bool,
    pub disarm_on_entry: bool,
    pub report_fd: i32,
    pub collector_reap_ms: i32,
    pub receiver_timeout_secs: u32,
    pub max_frames: usize,
    pub close_fds_on_receiver: bool,
    pub probe_seccomp: bool,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PrepareError {
    #[error("invalid signal-safe crashtracker configuration")]
    InvalidConfig,
    #[error("failed to prepare signal-safe crashtracker configuration")]
    Failed,
}

impl<'a> Default for SignalSafeInitConfig<'a> {
    fn default() -> Self {
        Self {
            receiver_path: &[],
            service: &[],
            env: &[],
            app_version: &[],
            runtime_id: &[],
            platform: &[],
            library_name: COMPAT_LIBRARY_NAME.as_bytes(),
            library_version: COMPAT_LIBRARY_VERSION.as_bytes(),
            family: COMPAT_LIBRARY_FAMILY.as_bytes(),
            default_service: COMPAT_DEFAULT_SERVICE.as_bytes(),
            force_on_top: false,
            only_bootstrap: false,
            debug_logging: false,
            create_alt_stack: false,
            use_alt_stack: false,
            block_signals: true,
            disarm_on_entry: false,
            report_fd: -1,
            collector_reap_ms: COLLECTOR_REAP_MS,
            receiver_timeout_secs: RECEIVER_TIMEOUT_SECS,
            max_frames: BACKTRACE_LEVELS_DEFAULT,
            close_fds_on_receiver: true,
            probe_seccomp: false,
        }
    }
}

#[derive(Serialize)]
struct WireConfig<'a> {
    additional_files: [&'a str; 0],
    create_alt_stack: bool,
    use_alt_stack: bool,
    demangle_names: bool,
    endpoint: Option<()>,
    resolve_frames: StacktraceCollection,
    signals: &'a [i32],
    timeout: WireTimeout,
    unix_socket_path: Option<()>,
}

#[derive(Serialize)]
struct WireTimeout {
    secs: u32,
    nanos: u32,
}

pub fn build_config_json(
    out: &mut HeaplessString<CONFIG_JSON_BUF_SIZE>,
    config: &SignalSafeInitConfig<'_>,
) -> bool {
    out.clear();
    let wire = WireConfig {
        additional_files: [],
        create_alt_stack: config.create_alt_stack,
        use_alt_stack: config.use_alt_stack,
        demangle_names: true,
        endpoint: None,
        resolve_frames: StacktraceCollection::EnabledWithSymbolsInReceiver,
        signals: &CRASH_SIGNALS,
        timeout: WireTimeout {
            secs: normalized_receiver_timeout_secs(config.receiver_timeout_secs),
            nanos: 0,
        },
        unix_socket_path: None,
    };

    let mut buf = [0u8; CONFIG_JSON_BUF_SIZE];
    let Ok(len) = serde_json_core::to_slice(&wire, &mut buf) else {
        return false;
    };
    let Ok(json) = core::str::from_utf8(&buf[..len]) else {
        return false;
    };
    out.push_str(json).is_ok() && out.push('\n').is_ok()
}

pub fn prepare_result(config: &SignalSafeInitConfig<'_>) -> Result<(), PrepareError> {
    validate(config)?;

    let m = meta_mut();
    if !build_config_json(&mut m.config_json, config) {
        return Err(PrepareError::Failed);
    }

    let mut metadata_truncated = false;
    metadata_truncated |= !set_str(&mut m.service, config.service);
    metadata_truncated |= !set_str(&mut m.env, config.env);
    metadata_truncated |= !set_str(&mut m.app_version, config.app_version);
    metadata_truncated |= !set_str(&mut m.runtime_id, config.runtime_id);
    metadata_truncated |= !set_str(&mut m.platform, config.platform);
    if m.platform.is_empty() {
        metadata_truncated |= !set_str(&mut m.platform, b"host");
    }
    metadata_truncated |= !set_str_or(
        &mut m.library_name,
        config.library_name,
        COMPAT_LIBRARY_NAME.as_bytes(),
    );
    metadata_truncated |= !set_str_or(
        &mut m.library_version,
        config.library_version,
        COMPAT_LIBRARY_VERSION.as_bytes(),
    );
    metadata_truncated |= !set_str_or(
        &mut m.family,
        config.family,
        COMPAT_LIBRARY_FAMILY.as_bytes(),
    );
    metadata_truncated |= !set_str_or(
        &mut m.default_service,
        config.default_service,
        COMPAT_DEFAULT_SERVICE.as_bytes(),
    );

    if !set_receiver_path(&mut m.process_path, config.receiver_path) {
        return Err(PrepareError::InvalidConfig);
    }

    state::FORCE_ON_TOP.store(config.force_on_top, Relaxed);
    state::ONLY_BOOTSTRAP.store(config.only_bootstrap, Relaxed);
    state::DEBUG_LOG.store(config.debug_logging, Relaxed);
    state::CREATE_ALT_STACK.store(config.create_alt_stack, Relaxed);
    state::USE_ALT_STACK.store(config.use_alt_stack, Relaxed);
    state::BLOCK_SIGNALS.store(config.block_signals, Relaxed);
    state::DISARM_ON_ENTRY.store(config.disarm_on_entry, Relaxed);
    state::CLOSE_FDS_ON_RECEIVER.store(config.close_fds_on_receiver, Relaxed);
    state::REPORT_FD.store(config.report_fd, Relaxed);
    state::COLLECTOR_REAP_MS.store(
        normalized_collector_reap_ms(config.collector_reap_ms),
        Relaxed,
    );
    state::RECEIVER_TIMEOUT_MS.store(
        normalized_receiver_timeout_secs(config.receiver_timeout_secs) as i32 * 1000
            + RECEIVER_TIMEOUT_GRACE_MS,
        Relaxed,
    );
    state::MAX_FRAMES.store(normalized_max_frames(config.max_frames), Relaxed);
    capabilities::publish(
        m.process_path.as_slice(),
        config.report_fd,
        config.probe_seccomp,
    );
    if metadata_truncated {
        capabilities::note_degraded(capabilities::DEGRADED_METADATA_TRUNCATED);
    }
    Ok(())
}

pub fn prepare_from_env_result() -> Result<(), PrepareError> {
    if disabled_by_env() {
        return Err(PrepareError::InvalidConfig);
    }

    // Prefer the neutral runtime receiver path name. DD_TRACE_C_CRASHTRACKER_PROCESS is
    // retained as a lower-priority compatibility alias for existing deployments.
    let receiver_path = env_get(b"DD_CRASHTRACKING_RECEIVER_PATH\0")
        .or_else(|| env_get(b"DD_TRACE_C_CRASHTRACKER_PROCESS\0"))
        .filter(|v| !v.is_empty())
        .unwrap_or(DEFAULT_RECEIVER_PATH.as_bytes());
    let platform = env_get(b"DD_INJECT_SENDER_TYPE\0")
        .filter(|v| !v.is_empty())
        .unwrap_or(b"host");
    let debug_logging = parse_log_level(env_get(b"DD_TRACE_LOG_LEVEL\0")) >= DD_LOG_DEBUG;

    prepare_result(&SignalSafeInitConfig {
        receiver_path,
        service: env_get(b"DD_SERVICE\0").unwrap_or(&[]),
        env: env_get(b"DD_ENV\0").unwrap_or(&[]),
        app_version: env_get(b"DD_VERSION\0").unwrap_or(&[]),
        runtime_id: env_get(b"DD_RUNTIME_ID\0").unwrap_or(&[]),
        platform,
        force_on_top: is_true(env_get(b"DD_CRASHTRACKING_ALWAYS_ON_TOP\0")),
        only_bootstrap: is_true(env_get(b"DD_CRASHTRACKING_ONLY_BOOTSTRAP\0")),
        debug_logging,
        probe_seccomp: is_true(env_get(b"DD_CRASHTRACKING_PROBE_SECCOMP\0")),
        ..SignalSafeInitConfig::default()
    })
}

pub fn disabled_by_env() -> bool {
    is_false(env_get(b"DD_CRASHTRACKING_ENABLED\0"))
}

fn normalized_receiver_timeout_secs(value: u32) -> u32 {
    if value == 0 {
        RECEIVER_TIMEOUT_SECS
    } else if value > RECEIVER_TIMEOUT_SECS_MAX {
        RECEIVER_TIMEOUT_SECS_MAX
    } else {
        value
    }
}

fn normalized_collector_reap_ms(value: i32) -> i32 {
    if value <= 0 {
        COLLECTOR_REAP_MS
    } else {
        value
    }
}

fn normalized_max_frames(value: usize) -> usize {
    if value == 0 {
        BACKTRACE_LEVELS_DEFAULT
    } else if value > BACKTRACE_LEVELS_MAX {
        BACKTRACE_LEVELS_MAX
    } else {
        value
    }
}

fn set_str<const N: usize>(dst: &mut HeaplessString<N>, src: &[u8]) -> bool {
    dst.clear();
    if let Ok(s) = core::str::from_utf8(src) {
        for ch in s.chars() {
            if dst.push(ch).is_err() {
                return false;
            }
        }
    }
    true
}

fn set_str_or<const N: usize>(dst: &mut HeaplessString<N>, src: &[u8], default: &[u8]) -> bool {
    if src.is_empty() {
        set_str(dst, default)
    } else {
        set_str(dst, src)
    }
}

fn set_receiver_path(dst: &mut heapless::Vec<u8, PATH_CAPACITY>, path: &[u8]) -> bool {
    dst.clear();
    let selected = if path.is_empty() {
        DEFAULT_RECEIVER_PATH.as_bytes()
    } else {
        path
    };
    if selected.len() >= dst.capacity() {
        return false;
    }
    dst.extend_from_slice(selected).is_ok() && dst.push(0).is_ok()
}

fn env_get(name_nul: &[u8]) -> Option<&'static [u8]> {
    sys::env_get(name_nul)
}

fn validate(config: &SignalSafeInitConfig<'_>) -> Result<(), PrepareError> {
    if config.create_alt_stack && !config.use_alt_stack {
        return Err(PrepareError::InvalidConfig);
    }

    let receiver_path = if config.receiver_path.is_empty() {
        DEFAULT_RECEIVER_PATH.as_bytes()
    } else {
        config.receiver_path
    };
    if receiver_path.len() >= PATH_CAPACITY {
        return Err(PrepareError::InvalidConfig);
    }

    if config.report_fd >= 0 && !sys::fd_valid(config.report_fd) {
        return Err(PrepareError::InvalidConfig);
    }

    Ok(())
}

fn is_false(v: Option<&[u8]>) -> bool {
    match v {
        Some(s) => s == b"0" || s.eq_ignore_ascii_case(b"false") || s.eq_ignore_ascii_case(b"f"),
        None => false,
    }
}

fn is_true(v: Option<&[u8]>) -> bool {
    match v {
        Some(s) => s == b"1" || s.eq_ignore_ascii_case(b"true") || s.eq_ignore_ascii_case(b"t"),
        None => false,
    }
}

const DD_LOG_INFO: i32 = 3;
const DD_LOG_DEBUG: i32 = 4;

fn parse_log_level(v: Option<&[u8]>) -> i32 {
    match v {
        None => DD_LOG_INFO,
        Some(s) => {
            const LEVELS: [(&[u8], i32); 6] = [
                (b"off", 0),
                (b"error", 1),
                (b"warn", 2),
                (b"info", 3),
                (b"debug", 4),
                (b"trace", 5),
            ];
            for (name, level) in LEVELS {
                if s == name {
                    return level;
                }
            }
            DD_LOG_INFO
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec::Vec;
    use std::format;

    #[test]
    fn config_json_contains_receiver_contract() {
        let mut out = HeaplessString::<CONFIG_JSON_BUF_SIZE>::new();
        assert!(build_config_json(
            &mut out,
            &SignalSafeInitConfig::default()
        ));
        let signals = CRASH_SIGNALS
            .iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            out.as_str(),
            format!(
                "{{\"additional_files\":[],\"create_alt_stack\":false,\"use_alt_stack\":false,\
                 \"demangle_names\":true,\"endpoint\":null,\
                 \"resolve_frames\":\"EnabledWithSymbolsInReceiver\",\
                 \"signals\":[{signals}],\"timeout\":{{\"secs\":5,\"nanos\":0}},\
                 \"unix_socket_path\":null}}\n"
            )
        );
        assert!(CRASH_SIGNALS.contains(&libc::SIGSEGV));
        assert!(CRASH_SIGNALS.contains(&libc::SIGABRT));
        assert!(CRASH_SIGNALS.contains(&libc::SIGBUS));
        assert!(CRASH_SIGNALS.contains(&libc::SIGILL));
        assert!(CRASH_SIGNALS.contains(&libc::SIGFPE));
    }

    #[test]
    fn timeout_seconds_are_clamped() {
        assert_eq!(normalized_receiver_timeout_secs(0), RECEIVER_TIMEOUT_SECS);
        assert_eq!(normalized_receiver_timeout_secs(1), 1);
        assert_eq!(
            normalized_receiver_timeout_secs(RECEIVER_TIMEOUT_SECS_MAX + 1),
            RECEIVER_TIMEOUT_SECS_MAX
        );
    }

    #[test]
    fn validate_rejects_pointless_alt_stack_configuration() {
        assert_eq!(
            validate(&SignalSafeInitConfig {
                create_alt_stack: true,
                use_alt_stack: false,
                ..SignalSafeInitConfig::default()
            }),
            Err(PrepareError::InvalidConfig)
        );
    }

    #[test]
    fn env_get_walks_environ_without_getenv() {
        let _guard = crate::collector_signal_safe::TEST_GLOBAL_LOCK
            .lock()
            .expect("test lock poisoned");

        std::env::set_var("DD_SIGNAL_SAFE_ENV_GET_TEST", "walked");
        assert_eq!(
            env_get(b"DD_SIGNAL_SAFE_ENV_GET_TEST\0"),
            Some(&b"walked"[..])
        );
        std::env::remove_var("DD_SIGNAL_SAFE_ENV_GET_TEST");
    }

    #[test]
    fn bool_and_log_parsing_matches_compatibility_inputs() {
        assert!(is_false(Some(b"FALSE")));
        assert!(is_false(Some(b"0")));
        assert!(!is_false(Some(b"true")));
        assert!(is_true(Some(b"TrUe")));
        assert!(is_true(Some(b"1")));
        assert_eq!(parse_log_level(Some(b"debug")), DD_LOG_DEBUG);
        assert_eq!(parse_log_level(Some(b"DEBUG")), DD_LOG_INFO);
    }

    #[test]
    fn prepare_caches_fixed_metadata() {
        let _guard = crate::collector_signal_safe::TEST_GLOBAL_LOCK
            .lock()
            .expect("test lock poisoned");

        assert!(prepare_result(&SignalSafeInitConfig {
            receiver_path: b"/tmp/receiver",
            service: b"svc",
            env: b"prod",
            app_version: b"1.2.3",
            runtime_id: b"rid",
            platform: b"host",
            force_on_top: true,
            only_bootstrap: true,
            debug_logging: true,
            ..SignalSafeInitConfig::default()
        })
        .is_ok());

        let meta = state::meta();
        assert_eq!(meta.service.as_str(), "svc");
        assert_eq!(meta.env.as_str(), "prod");
        assert_eq!(meta.app_version.as_str(), "1.2.3");
        assert_eq!(meta.runtime_id.as_str(), "rid");
        assert_eq!(meta.platform.as_str(), "host");
        assert_eq!(meta.process_path.as_slice(), b"/tmp/receiver\0");
        assert!(state::FORCE_ON_TOP.load(Relaxed));
        assert!(state::ONLY_BOOTSTRAP.load(Relaxed));
        assert!(state::DEBUG_LOG.load(Relaxed));
    }

    #[test]
    fn prepare_marks_metadata_truncation_degraded() {
        let _guard = crate::collector_signal_safe::TEST_GLOBAL_LOCK
            .lock()
            .expect("test lock poisoned");
        let oversized_service = "s".repeat(300);

        assert!(prepare_result(&SignalSafeInitConfig {
            receiver_path: b"/definitely/missing-signal-safe-receiver",
            service: oversized_service.as_bytes(),
            ..SignalSafeInitConfig::default()
        })
        .is_ok());

        assert!(capabilities::degradations().contains(capabilities::DEGRADED_METADATA_TRUNCATED));
    }
}
