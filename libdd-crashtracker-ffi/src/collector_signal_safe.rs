// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::{c_char, CStr};

use libdd_crashtracker::collector_signal_safe::{
    bootstrap_complete, init, init_from_env, set_stage, shutdown, SignalSafeInitConfig, Stage,
};

#[repr(C)]
pub struct SignalSafeConfig {
    pub receiver_path: *const c_char,
    pub service: *const c_char,
    pub env: *const c_char,
    pub app_version: *const c_char,
    pub runtime_id: *const c_char,
    pub platform: *const c_char,
    pub force_on_top: bool,
    pub only_bootstrap: bool,
    pub debug_logging: bool,
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
pub extern "C" fn ddog_crasht_signal_safe_init_from_env() -> bool {
    init_from_env()
}

/// Initialize the signal-safe crashtracker with explicitly provided metadata.
///
/// # Safety
/// `config` must be either null or point to a valid `SignalSafeConfig`. Any non-null C string
/// pointer inside `config` must point to a valid NUL-terminated string for the duration of this
/// call. The strings are copied before the function returns.
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_signal_safe_init(config: *const SignalSafeConfig) -> bool {
    let Some(config) = config.as_ref() else {
        return false;
    };

    init(&SignalSafeInitConfig {
        receiver_path: cstr_bytes(config.receiver_path),
        service: cstr_bytes(config.service),
        env: cstr_bytes(config.env),
        app_version: cstr_bytes(config.app_version),
        runtime_id: cstr_bytes(config.runtime_id),
        platform: cstr_bytes(config.platform),
        force_on_top: config.force_on_top,
        only_bootstrap: config.only_bootstrap,
        debug_logging: config.debug_logging,
    })
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

unsafe fn cstr_bytes<'a>(ptr: *const c_char) -> &'a [u8] {
    if ptr.is_null() {
        &[]
    } else {
        CStr::from_ptr(ptr).to_bytes()
    }
}
