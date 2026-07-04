// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::ffi::c_char;
use core::fmt::Write;
use core::sync::atomic::Ordering::Relaxed;

use heapless::String as HeaplessString;

use super::state::{self, meta_mut};

pub const TRACE_C_VERSION: &str = match option_env!("DD_TRACE_C_VERSION") {
    Some(v) => v,
    None => "dev",
};

const DEFAULT_RECEIVER_PATH: &str = match option_env!("DD_TRACE_C_CRASHTRACKER_PROCESS_PATH") {
    Some(p) => p,
    None => "/opt/datadog-packages/datadog-apm-library-c/stable/process-crash-receiver",
};

pub const RECEIVER_TIMEOUT_SECS: u32 = 5;

pub const CRASH_SIGNALS: [i32; 5] = [
    libc::SIGSEGV,
    libc::SIGABRT,
    libc::SIGBUS,
    libc::SIGILL,
    libc::SIGFPE,
];

pub const CONFIG_JSON_BUF_SIZE: usize = 2048;

#[derive(Clone, Copy, Debug, Default)]
pub struct SignalSafeInitConfig<'a> {
    pub receiver_path: &'a [u8],
    pub service: &'a [u8],
    pub env: &'a [u8],
    pub app_version: &'a [u8],
    pub runtime_id: &'a [u8],
    pub platform: &'a [u8],
    pub force_on_top: bool,
    pub only_bootstrap: bool,
    pub debug_logging: bool,
}

pub fn build_config_json(out: &mut HeaplessString<CONFIG_JSON_BUF_SIZE>) -> bool {
    out.clear();
    if out
        .push_str(
            "{\"additional_files\":[],\
             \"create_alt_stack\":false,\
             \"use_alt_stack\":false,\
             \"demangle_names\":true,\
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
        "],\"timeout\":{{\"secs\":{RECEIVER_TIMEOUT_SECS},\"nanos\":0}},\"unix_socket_path\":null}}"
    )
    .is_ok()
}

pub fn prepare(config: &SignalSafeInitConfig<'_>) -> bool {
    let m = meta_mut();
    if !build_config_json(&mut m.config_json) {
        return false;
    }

    set_str(&mut m.service, config.service);
    set_str(&mut m.env, config.env);
    set_str(&mut m.app_version, config.app_version);
    set_str(&mut m.runtime_id, config.runtime_id);
    set_str(&mut m.platform, config.platform);
    if m.platform.is_empty() {
        set_str(&mut m.platform, b"host");
    }

    if !set_receiver_path(&mut m.process_path, config.receiver_path) {
        return false;
    }

    state::FORCE_ON_TOP.store(config.force_on_top, Relaxed);
    state::ONLY_BOOTSTRAP.store(config.only_bootstrap, Relaxed);
    state::DEBUG_LOG.store(config.debug_logging, Relaxed);
    true
}

pub fn prepare_from_env() -> bool {
    if is_false(env_get(b"DD_CRASHTRACKING_ENABLED\0")) {
        return false;
    }

    let receiver_path = env_get(b"DD_TRACE_C_CRASHTRACKER_PROCESS\0")
        .filter(|v| !v.is_empty())
        .unwrap_or(DEFAULT_RECEIVER_PATH.as_bytes());
    let platform = env_get(b"DD_INJECT_SENDER_TYPE\0")
        .filter(|v| !v.is_empty())
        .unwrap_or(b"host");
    let debug_logging = parse_log_level(env_get(b"DD_TRACE_LOG_LEVEL\0")) >= DD_LOG_DEBUG;

    prepare(&SignalSafeInitConfig {
        receiver_path,
        service: env_get(b"DD_SERVICE\0").unwrap_or(&[]),
        env: env_get(b"DD_ENV\0").unwrap_or(&[]),
        app_version: env_get(b"DD_VERSION\0").unwrap_or(&[]),
        runtime_id: env_get(b"DD_RUNTIME_ID\0").unwrap_or(&[]),
        platform,
        force_on_top: is_true(env_get(b"DD_CRASHTRACKING_ALWAYS_ON_TOP\0")),
        only_bootstrap: is_true(env_get(b"DD_CRASHTRACKING_ONLY_BOOTSTRAP\0")),
        debug_logging,
    })
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

pub unsafe fn cstr_bytes<'a>(p: *const c_char) -> &'a [u8] {
    let mut len = 0usize;
    while core::ptr::read_volatile(p.add(len)) != 0 {
        len += 1;
    }
    core::slice::from_raw_parts(p.cast(), len)
}

fn env_get(name_nul: &[u8]) -> Option<&'static [u8]> {
    let p = unsafe { libc::getenv(name_nul.as_ptr().cast()) };
    if p.is_null() {
        None
    } else {
        Some(unsafe { cstr_bytes(p) })
    }
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
        assert!(build_config_json(&mut out));
        assert!(out.contains("\"additional_files\":[]"));
        assert!(out.contains("\"resolve_frames\":\"EnabledWithSymbolsInReceiver\""));
        assert!(out.contains("\"unix_socket_path\":null"));
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn bool_and_log_parsing_matches_dd_trace_c() {
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
