// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{c_char, CStr};

use libdd_crashtracker::collector_signal_safe::{
    bootstrap_complete, capability_bits, init_from_env_result, init_result, owned_signal_count,
    owns_signal, set_stage, shutdown, InitResult, SignalSafeInitConfig, Stage,
};

#[repr(C)]
pub struct SignalSafeConfig {
    pub receiver_path: *const c_char,
    pub service: *const c_char,
    pub env: *const c_char,
    pub app_version: *const c_char,
    pub runtime_id: *const c_char,
    pub platform: *const c_char,
    pub library_name: *const c_char,
    pub library_version: *const c_char,
    pub family: *const c_char,
    pub default_service: *const c_char,
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

#[repr(C)]
#[derive(Clone, Copy)]
pub enum SignalSafeInitResult {
    Enabled = 0,
    DisabledByConfig = 1,
    Failed = 2,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub enum SignalSafeStage {
    Uninitialized = 0,
    CrashtrackerInit = 1,
    PlatformInit = 2,
    LanguageInit = 3,
    PluginLoading = 4,
    InjectionMetadataSend = 5,
    HttpClientSend = 6,
    Application = 7,
    CrashtrackerUninstall = 8,
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_init_from_env() -> SignalSafeInitResult {
    init_result_to_ffi(init_from_env_result())
}

/// Initialize the signal-safe crashtracker with explicitly provided metadata.
///
/// # Safety
/// `config` must be either null or point to a valid `SignalSafeConfig`. Any non-null C string
/// pointer inside `config` must point to a valid NUL-terminated string for the duration of this
/// call. The strings are copied before the function returns.
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_signal_safe_init(
    config: *const SignalSafeConfig,
) -> SignalSafeInitResult {
    let Some(config) = config.as_ref() else {
        return SignalSafeInitResult::Failed;
    };

    init_result_to_ffi(init_result(&SignalSafeInitConfig {
        receiver_path: cstr_bytes(config.receiver_path),
        service: cstr_bytes(config.service),
        env: cstr_bytes(config.env),
        app_version: cstr_bytes(config.app_version),
        runtime_id: cstr_bytes(config.runtime_id),
        platform: cstr_bytes(config.platform),
        library_name: cstr_bytes(config.library_name),
        library_version: cstr_bytes(config.library_version),
        family: cstr_bytes(config.family),
        default_service: cstr_bytes(config.default_service),
        force_on_top: config.force_on_top,
        only_bootstrap: config.only_bootstrap,
        debug_logging: config.debug_logging,
        create_alt_stack: config.create_alt_stack,
        use_alt_stack: config.use_alt_stack,
        block_signals: config.block_signals,
        disarm_on_entry: config.disarm_on_entry,
        report_fd: config.report_fd,
        collector_reap_ms: config.collector_reap_ms,
        receiver_timeout_secs: config.receiver_timeout_secs,
        max_frames: config.max_frames,
        close_fds_on_receiver: config.close_fds_on_receiver,
    }))
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_bootstrap_complete() {
    bootstrap_complete();
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_shutdown() {
    shutdown();
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_set_stage(stage: SignalSafeStage) {
    set_stage(match stage {
        SignalSafeStage::Uninitialized => Stage::Uninitialized,
        SignalSafeStage::CrashtrackerInit => Stage::CrashtrackerInit,
        SignalSafeStage::PlatformInit => Stage::PlatformInit,
        SignalSafeStage::LanguageInit => Stage::LanguageInit,
        SignalSafeStage::PluginLoading => Stage::PluginLoading,
        SignalSafeStage::InjectionMetadataSend => Stage::InjectionMetadataSend,
        SignalSafeStage::HttpClientSend => Stage::HttpClientSend,
        SignalSafeStage::Application => Stage::Application,
        SignalSafeStage::CrashtrackerUninstall => Stage::CrashtrackerUninstall,
    });
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_capabilities() -> u32 {
    capability_bits()
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_owned_signal_count() -> u32 {
    owned_signal_count()
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_owns_signal(signum: i32) -> bool {
    owns_signal(signum)
}

fn init_result_to_ffi(result: InitResult) -> SignalSafeInitResult {
    match result {
        InitResult::Enabled => SignalSafeInitResult::Enabled,
        InitResult::DisabledByConfig => SignalSafeInitResult::DisabledByConfig,
        InitResult::Failed => SignalSafeInitResult::Failed,
    }
}

unsafe fn cstr_bytes<'a>(ptr: *const c_char) -> &'a [u8] {
    if ptr.is_null() {
        &[]
    } else {
        CStr::from_ptr(ptr).to_bytes()
    }
}
