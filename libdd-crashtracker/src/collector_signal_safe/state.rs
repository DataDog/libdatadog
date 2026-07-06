// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::mem::MaybeUninit;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicUsize, Ordering};

use heapless::{String as HeaplessString, Vec as HeaplessVec};

use super::config::CONFIG_JSON_BUF_SIZE;

// Raw signal numbers index these arrays. 128 covers Linux and BSD/macOS signal ranges used here.
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
    pub library_name: HeaplessString<128>,
    pub library_version: HeaplessString<128>,
    pub family: HeaplessString<128>,
    pub default_service: HeaplessString<128>,
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
            library_name: HeaplessString::new(),
            library_version: HeaplessString::new(),
            family: HeaplessString::new(),
            default_service: HeaplessString::new(),
        }
    }
}

struct StaticMeta(UnsafeCell<Meta>);

unsafe impl Sync for StaticMeta {}

static META: StaticMeta = StaticMeta(UnsafeCell::new(Meta::new()));

const INIT_UNINIT: i32 = 0;
const INIT_INITIALIZING: i32 = 1;
const INIT_READY: i32 = 2;

static INIT_STATE: AtomicI32 = AtomicI32::new(INIT_UNINIT);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BeginInitError {
    AlreadyInitialized,
    Busy,
}

pub fn begin_init() -> Result<(), BeginInitError> {
    match INIT_STATE.compare_exchange(
        INIT_UNINIT,
        INIT_INITIALIZING,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => Ok(()),
        Err(INIT_READY) => Err(BeginInitError::AlreadyInitialized),
        Err(_) => Err(BeginInitError::Busy),
    }
}

pub fn finish_init() {
    INIT_STATE.store(INIT_READY, Ordering::Release);
}

pub fn fail_init() {
    INIT_STATE.store(INIT_UNINIT, Ordering::Release);
}

pub fn reset_init() {
    INIT_STATE.store(INIT_UNINIT, Ordering::Release);
}

pub fn meta() -> &'static Meta {
    unsafe { &*META.0.get() }
}

pub fn meta_mut() -> &'static mut Meta {
    unsafe { &mut *META.0.get() }
}

pub static ORIG_FN: [AtomicPtr<c_void>; NSIG] = [const { AtomicPtr::new(null_mut()) }; NSIG];
pub static ORIG_FLAGS: [AtomicI32; NSIG] = [const { AtomicI32::new(0) }; NSIG];
pub static OWN_SIGNAL: [AtomicBool; NSIG] = [const { AtomicBool::new(false) }; NSIG];
pub static APP_HANDLER_PRESENT: [AtomicBool; NSIG] = [const { AtomicBool::new(false) }; NSIG];

struct SigMaskStorage(UnsafeCell<[MaybeUninit<libc::sigset_t>; NSIG]>);

unsafe impl Sync for SigMaskStorage {}

static ORIG_MASKS: SigMaskStorage =
    SigMaskStorage(UnsafeCell::new([const { MaybeUninit::uninit() }; NSIG]));

pub fn store_orig_mask(idx: usize, mask: &libc::sigset_t) {
    unsafe {
        (*ORIG_MASKS.0.get())[idx].as_mut_ptr().write(*mask);
    }
}

pub fn load_orig_mask(idx: usize, out: &mut libc::sigset_t) {
    unsafe {
        out.clone_from(&*(*ORIG_MASKS.0.get())[idx].as_ptr());
    }
}

pub static HANDLERS_ENABLED: AtomicBool = AtomicBool::new(false);
pub static FORCE_ON_TOP: AtomicBool = AtomicBool::new(false);
pub static ONLY_BOOTSTRAP: AtomicBool = AtomicBool::new(false);
pub static DEBUG_LOG: AtomicBool = AtomicBool::new(false);
pub static INSTALLED: AtomicBool = AtomicBool::new(false);
pub static CREATE_ALT_STACK: AtomicBool = AtomicBool::new(false);
pub static USE_ALT_STACK: AtomicBool = AtomicBool::new(false);
pub static BLOCK_SIGNALS: AtomicBool = AtomicBool::new(true);
pub static DISARM_ON_ENTRY: AtomicBool = AtomicBool::new(false);
pub static CLOSE_FDS_ON_RECEIVER: AtomicBool = AtomicBool::new(true);
pub static REPORT_FD: AtomicI32 = AtomicI32::new(-1);
pub static COLLECTOR_REAP_MS: AtomicI32 = AtomicI32::new(500);
pub static RECEIVER_TIMEOUT_MS: AtomicI32 = AtomicI32::new(6_000);
pub static MAX_FRAMES: AtomicUsize = AtomicUsize::new(32);

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
    STAGE.store(stage as i32, Ordering::Relaxed);
}

pub fn current_stage_name() -> &'static str {
    match STAGE.load(Ordering::Relaxed) {
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
        ORIG_FN[i].store(null_mut(), Ordering::Relaxed);
        ORIG_FLAGS[i].store(0, Ordering::Relaxed);
        OWN_SIGNAL[i].store(false, Ordering::Relaxed);
        APP_HANDLER_PRESENT[i].store(false, Ordering::Relaxed);
        i += 1;
    }
}

pub fn owns_signal(sig: i32) -> bool {
    sig_index(sig)
        .map(|i| OWN_SIGNAL[i].load(Ordering::Acquire))
        .unwrap_or(false)
}

pub fn owned_signal_count() -> u32 {
    let mut count = 0u32;
    let mut i = 0usize;
    while i < NSIG {
        if OWN_SIGNAL[i].load(Ordering::Acquire) {
            count += 1;
        }
        i += 1;
    }
    count
}

pub fn app_handler_present(sig: i32) -> bool {
    sig_index(sig)
        .map(|i| APP_HANDLER_PRESENT[i].load(Ordering::Acquire))
        .unwrap_or(false)
}
