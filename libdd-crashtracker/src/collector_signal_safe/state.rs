// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::mem::MaybeUninit;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicUsize, Ordering};

use heapless::{String as HeaplessString, Vec as HeaplessVec};
use thiserror::Error;

use super::config::{CONFIG_JSON_BUF_SIZE, PATH_CAPACITY};

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
    pub process_path: HeaplessVec<u8, PATH_CAPACITY>,
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

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BeginInitError {
    #[error("signal-safe crashtracker is already initialized")]
    AlreadyInitialized,
    #[error("signal-safe crashtracker initialization is already in progress")]
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

pub fn reset_init() {
    INIT_STATE.store(INIT_UNINIT, Ordering::Release);
}

pub fn meta() -> &'static Meta {
    unsafe { &*META.0.get() }
}

pub fn meta_mut() -> &'static mut Meta {
    unsafe { &mut *META.0.get() }
}

pub(super) struct SignalSlot {
    orig_fn: AtomicPtr<c_void>,
    orig_flags: AtomicI32,
    own_signal: AtomicBool,
    app_handler_present: AtomicBool,
    orig_mask: UnsafeCell<MaybeUninit<libc::sigset_t>>,
}

unsafe impl Sync for SignalSlot {}

impl SignalSlot {
    const fn new() -> Self {
        Self {
            orig_fn: AtomicPtr::new(null_mut()),
            orig_flags: AtomicI32::new(0),
            own_signal: AtomicBool::new(false),
            app_handler_present: AtomicBool::new(false),
            orig_mask: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    pub(super) fn original_handler(&self) -> (*mut c_void, i32) {
        (
            self.orig_fn.load(Ordering::Relaxed),
            self.orig_flags.load(Ordering::Relaxed),
        )
    }

    pub(super) fn store_original_handler(
        &self,
        fn_ptr: *mut c_void,
        flags: i32,
        mask: &libc::sigset_t,
    ) {
        self.orig_fn.store(fn_ptr, Ordering::Relaxed);
        self.orig_flags.store(flags, Ordering::Relaxed);
        unsafe {
            (*self.orig_mask.get()).as_mut_ptr().write(*mask);
        }
    }

    pub(super) fn load_original_mask(&self, out: &mut libc::sigset_t) {
        unsafe {
            out.clone_from(&*(*self.orig_mask.get()).as_ptr());
        }
    }

    pub(super) fn set_owned(&self, owned: bool) {
        self.own_signal.store(owned, Ordering::Relaxed);
    }

    pub(super) fn owns_signal(&self) -> bool {
        self.own_signal.load(Ordering::Acquire)
    }

    pub(super) fn set_app_handler_present(&self) {
        self.app_handler_present.store(true, Ordering::Relaxed);
    }

    pub(super) fn app_handler_present(&self) -> bool {
        self.app_handler_present.load(Ordering::Acquire)
    }

    fn clear(&self) {
        self.orig_fn.store(null_mut(), Ordering::Relaxed);
        self.orig_flags.store(0, Ordering::Relaxed);
        self.own_signal.store(false, Ordering::Relaxed);
        self.app_handler_present.store(false, Ordering::Relaxed);
    }
}

static SIGNAL_SLOTS: [SignalSlot; NSIG] = [const { SignalSlot::new() }; NSIG];

pub(super) fn signal_slot(idx: usize) -> &'static SignalSlot {
    &SIGNAL_SLOTS[idx]
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

pub fn clear_signal_state() {
    for slot in &SIGNAL_SLOTS {
        slot.clear();
    }
}

pub fn owns_signal(sig: i32) -> bool {
    sig_index(sig)
        .map(|i| signal_slot(i).owns_signal())
        .unwrap_or(false)
}

pub fn owned_signal_count() -> u32 {
    SIGNAL_SLOTS
        .iter()
        .filter(|slot| slot.owns_signal())
        .count() as u32
}

pub fn app_handler_present(sig: i32) -> bool {
    sig_index(sig)
        .map(|i| signal_slot(i).app_handler_present())
        .unwrap_or(false)
}
