// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::ffi::c_void;
use core::ptr::{addr_of, addr_of_mut, null_mut};
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering::Relaxed};

use heapless::{String as HeaplessString, Vec as HeaplessVec};

use super::config::CONFIG_JSON_BUF_SIZE;

pub const NSIG: usize = 128;

#[inline]
pub fn sig_index(sig: i32) -> Option<usize> {
    usize::try_from(sig).ok().filter(|&i| i < NSIG)
}

pub struct Meta {
    pub config_json: HeaplessString<CONFIG_JSON_BUF_SIZE>,
    pub service: HeaplessString<256>,
    pub env: HeaplessString<256>,
    pub app_version: HeaplessString<256>,
    pub platform: HeaplessString<256>,
    pub runtime_id: HeaplessString<64>,
    pub process_path: HeaplessVec<u8, 513>,
}

impl Meta {
    const fn new() -> Self {
        Self {
            config_json: HeaplessString::new(),
            service: HeaplessString::new(),
            env: HeaplessString::new(),
            app_version: HeaplessString::new(),
            platform: HeaplessString::new(),
            runtime_id: HeaplessString::new(),
            process_path: HeaplessVec::new(),
        }
    }
}

static mut META: Meta = Meta::new();

pub fn meta() -> &'static Meta {
    unsafe { &*addr_of!(META) }
}

pub fn meta_mut() -> &'static mut Meta {
    unsafe { &mut *addr_of_mut!(META) }
}

pub static ORIG_FN: [AtomicPtr<c_void>; NSIG] = [const { AtomicPtr::new(null_mut()) }; NSIG];
pub static ORIG_FLAGS: [AtomicI32; NSIG] = [const { AtomicI32::new(0) }; NSIG];
pub static OWN_SIGNAL: [AtomicBool; NSIG] = [const { AtomicBool::new(false) }; NSIG];

pub static HANDLERS_ENABLED: AtomicBool = AtomicBool::new(false);
pub static FORCE_ON_TOP: AtomicBool = AtomicBool::new(false);
pub static ONLY_BOOTSTRAP: AtomicBool = AtomicBool::new(false);
pub static DEBUG_LOG: AtomicBool = AtomicBool::new(false);
pub static INSTALLED: AtomicBool = AtomicBool::new(false);

#[repr(i32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
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

static STAGE: AtomicI32 = AtomicI32::new(Stage::Uninitialized as i32);

pub fn set_stage(stage: Stage) {
    STAGE.store(stage as i32, Relaxed);
}

pub fn current_stage_name() -> &'static str {
    match STAGE.load(Relaxed) {
        1 => "crashtracker_init",
        2 => "platform_init",
        3 => "language_init",
        4 => "plugin_loading",
        5 => "injection_metadata_send",
        6 => "http_client_send",
        7 => "application",
        8 => "crashtracker_uninstall",
        _ => "uninitialized",
    }
}

pub fn clear_signal_state() {
    let mut i = 0usize;
    while i < NSIG {
        ORIG_FN[i].store(null_mut(), Relaxed);
        ORIG_FLAGS[i].store(0, Relaxed);
        OWN_SIGNAL[i].store(false, Relaxed);
        i += 1;
    }
}
