// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::ffi::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};

use libdd_crashtracker::collector_signal_safe::{
    bootstrap_complete, capability_bits, cstr_bytes_bounded, degradation_bits,
    init_from_env_result, init_result, owned_signal_count, owns_signal, set_stage, shutdown,
    InitResult, SignalSafeInitConfig, Stage,
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
    /// Installs the built-in alternate signal stack on the init thread only.
    ///
    /// Signal alternate stacks are per-thread kernel state. Stack-overflow crashes on other
    /// threads require those threads to install their own alternate stacks.
    pub create_alt_stack: bool,
    /// Registers crash handlers with SA_ONSTACK.
    ///
    /// This may be used with create_alt_stack or with a caller-provided alternate stack already
    /// installed on the current thread.
    pub use_alt_stack: bool,
    /// Runs app handlers invoked from the signal-safe handler with managed crash signals blocked.
    pub block_signals: bool,
    pub disarm_on_entry: bool,
    pub report_fd: i32,
    pub collector_reap_ms: i32,
    pub receiver_timeout_secs: u32,
    pub max_frames: usize,
    pub close_fds_on_receiver: bool,
    pub probe_seccomp: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub enum SignalSafeInitResult {
    Enabled = 0,
    DisabledByConfig = 1,
    Failed = 2,
    AlreadyInitialized = 3,
    OwnerConflict = 4,
    InvalidConfig = 5,
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

impl From<SignalSafeStage> for Stage {
    fn from(stage: SignalSafeStage) -> Self {
        match stage {
            SignalSafeStage::Uninitialized => Stage::Uninitialized,
            SignalSafeStage::CrashtrackerInit => Stage::CrashtrackerInit,
            SignalSafeStage::PlatformInit => Stage::PlatformInit,
            SignalSafeStage::LanguageInit => Stage::LanguageInit,
            SignalSafeStage::PluginLoading => Stage::PluginLoading,
            SignalSafeStage::InjectionMetadataSend => Stage::InjectionMetadataSend,
            SignalSafeStage::HttpClientSend => Stage::HttpClientSend,
            SignalSafeStage::Application => Stage::Application,
            SignalSafeStage::CrashtrackerUninstall => Stage::CrashtrackerUninstall,
        }
    }
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_init_from_env() -> SignalSafeInitResult {
    ffi_result(|| init_result_to_ffi(init_from_env_result()))
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
    ffi_result(|| {
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
            probe_seccomp: config.probe_seccomp,
        }))
    })
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_bootstrap_complete() {
    ffi_void(bootstrap_complete);
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_shutdown() {
    ffi_void(shutdown);
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_set_stage(stage: SignalSafeStage) {
    ffi_void(|| set_stage(stage.into()));
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_capabilities() -> u32 {
    ffi_u32(capability_bits)
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_degradations() -> u32 {
    ffi_u32(degradation_bits)
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_owned_signal_count() -> u32 {
    ffi_u32(owned_signal_count)
}

#[no_mangle]
pub extern "C" fn ddog_crasht_signal_safe_owns_signal(signum: i32) -> bool {
    catch_unwind(AssertUnwindSafe(|| owns_signal(signum))).unwrap_or(false)
}

fn init_result_to_ffi(result: InitResult) -> SignalSafeInitResult {
    match result {
        InitResult::Enabled => SignalSafeInitResult::Enabled,
        InitResult::DisabledByConfig => SignalSafeInitResult::DisabledByConfig,
        InitResult::Failed => SignalSafeInitResult::Failed,
        InitResult::AlreadyInitialized => SignalSafeInitResult::AlreadyInitialized,
        InitResult::OwnerConflict => SignalSafeInitResult::OwnerConflict,
        InitResult::InvalidConfig => SignalSafeInitResult::InvalidConfig,
    }
}

fn ffi_result(f: impl FnOnce() -> SignalSafeInitResult) -> SignalSafeInitResult {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(SignalSafeInitResult::Failed)
}

fn ffi_void(f: impl FnOnce()) {
    let _ = catch_unwind(AssertUnwindSafe(f));
}

fn ffi_u32(f: impl FnOnce() -> u32) -> u32 {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(0)
}

unsafe fn cstr_bytes<'a>(ptr: *const c_char) -> &'a [u8] {
    unsafe { cstr_bytes_bounded(ptr) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_init_result_values_match_library_values() {
        assert_eq!(
            SignalSafeInitResult::Enabled as i32,
            InitResult::Enabled as i32
        );
        assert_eq!(
            SignalSafeInitResult::DisabledByConfig as i32,
            InitResult::DisabledByConfig as i32
        );
        assert_eq!(
            SignalSafeInitResult::Failed as i32,
            InitResult::Failed as i32
        );
        assert_eq!(
            SignalSafeInitResult::AlreadyInitialized as i32,
            InitResult::AlreadyInitialized as i32
        );
        assert_eq!(
            SignalSafeInitResult::OwnerConflict as i32,
            InitResult::OwnerConflict as i32
        );
        assert_eq!(
            SignalSafeInitResult::InvalidConfig as i32,
            InitResult::InvalidConfig as i32
        );
    }
}
