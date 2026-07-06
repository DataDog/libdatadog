// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::fmt::Write;
use core::sync::atomic::Ordering::Relaxed;

use heapless::String as HeaplessString;

use super::state::meta_mut;
use super::{capabilities, state, sys};

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

pub const RECEIVER_TIMEOUT_SECS: u32 = 5;
pub const RECEIVER_TIMEOUT_SECS_MAX: u32 = 60;
pub const COLLECTOR_REAP_MS: i32 = 500;
pub const RECEIVER_TIMEOUT_GRACE_MS: i32 = 1000;
pub const BACKTRACE_LEVELS_DEFAULT: usize = 32;
pub const BACKTRACE_LEVELS_MAX: usize = 64;

pub const CRASH_SIGNALS: [i32; 5] = [
    libc::SIGSEGV,
    libc::SIGABRT,
    libc::SIGBUS,
    libc::SIGILL,
    libc::SIGFPE,
];

pub const CONFIG_JSON_BUF_SIZE: usize = 2048;

#[derive(Clone, Copy, Debug)]
pub struct SignalSafeInitConfig<'a> {
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
    pub create_alt_stack: bool,
    pub use_alt_stack: bool,
    pub block_signals: bool,
    pub disarm_on_entry: bool,
    pub report_fd: i32,
    pub collector_reap_ms: i32,
    pub receiver_timeout_secs: u32,
    pub max_frames: usize,
    pub close_fds_on_receiver: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrepareError {
    InvalidConfig,
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
        }
    }
}

pub fn build_config_json(
    out: &mut HeaplessString<CONFIG_JSON_BUF_SIZE>,
    config: &SignalSafeInitConfig<'_>,
) -> bool {
    out.clear();
    if out
        .push_str("{\"additional_files\":[],\"create_alt_stack\":")
        .is_err()
    {
        return false;
    }
    if write!(out, "{}", config.create_alt_stack).is_err()
        || out.push_str(",\"use_alt_stack\":").is_err()
        || write!(out, "{}", config.use_alt_stack).is_err()
        || out
            .push_str(
                ",\"demangle_names\":true,\
                 \"endpoint\":null,\
                 \"resolve_frames\":\"EnabledWithSymbolsInReceiver\",\
                 \"signals\":[",
            )
            .is_err()
    {
        return false;
    }

    for (i, sig) in CRASH_SIGNALS.iter().enumerate() {
        if i > 0 && out.push(',').is_err() {
            return false;
        }
        if write!(out, "{sig}").is_err() {
            return false;
        }
    }

    writeln!(
        out,
        "],\"timeout\":{{\"secs\":{},\"nanos\":0}},\"unix_socket_path\":null}}",
        normalized_receiver_timeout_secs(config.receiver_timeout_secs)
    )
    .is_ok()
}

pub fn prepare(config: &SignalSafeInitConfig<'_>) -> bool {
    prepare_result(config).is_ok()
}

pub fn prepare_result(config: &SignalSafeInitConfig<'_>) -> Result<(), PrepareError> {
    validate(config)?;

    let m = meta_mut();
    if !build_config_json(&mut m.config_json, config) {
        return Err(PrepareError::Failed);
    }

    set_str(&mut m.service, config.service);
    set_str(&mut m.env, config.env);
    set_str(&mut m.app_version, config.app_version);
    set_str(&mut m.runtime_id, config.runtime_id);
    set_str(&mut m.platform, config.platform);
    if m.platform.is_empty() {
        set_str(&mut m.platform, b"host");
    }
    set_str_or(
        &mut m.library_name,
        config.library_name,
        COMPAT_LIBRARY_NAME.as_bytes(),
    );
    set_str_or(
        &mut m.library_version,
        config.library_version,
        COMPAT_LIBRARY_VERSION.as_bytes(),
    );
    set_str_or(
        &mut m.family,
        config.family,
        COMPAT_LIBRARY_FAMILY.as_bytes(),
    );
    set_str_or(
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
    capabilities::publish(m.process_path.as_slice(), config.report_fd);
    Ok(())
}

pub fn prepare_from_env() -> bool {
    prepare_from_env_result().is_ok()
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

fn set_str<const N: usize>(dst: &mut HeaplessString<N>, src: &[u8]) {
    dst.clear();
    if let Ok(s) = core::str::from_utf8(src) {
        for ch in s.chars() {
            if dst.push(ch).is_err() {
                break;
            }
        }
    }
}

fn set_str_or<const N: usize>(dst: &mut HeaplessString<N>, src: &[u8], default: &[u8]) {
    if src.is_empty() {
        set_str(dst, default);
    } else {
        set_str(dst, src);
    }
}

fn set_receiver_path(dst: &mut heapless::Vec<u8, 513>, path: &[u8]) -> bool {
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
    if receiver_path.len() >= 513 {
        return Err(PrepareError::InvalidConfig);
    }

    if config.report_fd >= 0 && !sys::fd_valid(config.report_fd) {
        return Err(PrepareError::InvalidConfig);
    }

    Ok(())
}

fn eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0usize;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn eq_ic(a: &[u8], lower: &[u8]) -> bool {
    if a.len() != lower.len() {
        return false;
    }
    let mut i = 0usize;
    while i < a.len() {
        if a[i].to_ascii_lowercase() != lower[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn is_false(v: Option<&[u8]>) -> bool {
    match v {
        Some(s) => eq(s, b"0") || eq_ic(s, b"false") || eq_ic(s, b"f"),
        None => false,
    }
}

fn is_true(v: Option<&[u8]>) -> bool {
    match v {
        Some(s) => eq(s, b"1") || eq_ic(s, b"true") || eq_ic(s, b"t"),
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
                if eq(s, name) {
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

    #[test]
    fn config_json_contains_receiver_contract() {
        let mut out = HeaplessString::<CONFIG_JSON_BUF_SIZE>::new();
        assert!(build_config_json(
            &mut out,
            &SignalSafeInitConfig::default()
        ));
        assert!(out.contains("\"additional_files\":[]"));
        assert!(out.contains("\"resolve_frames\":\"EnabledWithSymbolsInReceiver\""));
        assert!(out.contains("\"unix_socket_path\":null"));
        assert!(out.ends_with('\n'));
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

        assert!(prepare(&SignalSafeInitConfig {
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
        }));

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
}
