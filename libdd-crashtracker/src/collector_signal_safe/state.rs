// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Global state publication for the signal-safe collector.
//!
//! Initialization is single-threaded behind [`InitSession`]. The initializing thread writes
//! metadata and settings first, then publishes handler readiness with the `Release` store to
//! [`HANDLERS_ENABLED`]. The signal handler `Acquire`-loads that flag before reading the rest of
//! the state, so per-setting and per-slot accesses can stay `Relaxed`.

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::mem::MaybeUninit;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicUsize, Ordering};

use heapless::{String as HeaplessString, Vec as HeaplessVec};
use thiserror::Error;

use super::config::{self, CONFIG_JSON_BUF_SIZE, PATH_CAPACITY};

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
    pub(super) const fn new() -> Self {
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

pub struct InitSession {
    finished: bool,
}

impl InitSession {
    pub fn meta_mut(&mut self) -> &mut Meta {
        unsafe { &mut *META.0.get() }
    }

    pub fn finish(mut self) {
        self.finished = true;
        INIT_STATE.store(INIT_READY, Ordering::Release);
    }
}

impl Drop for InitSession {
    fn drop(&mut self) {
        if !self.finished {
            INIT_STATE.store(INIT_UNINIT, Ordering::Release);
        }
    }
}

pub fn begin_init() -> Result<InitSession, BeginInitError> {
    match INIT_STATE.compare_exchange(
        INIT_UNINIT,
        INIT_INITIALIZING,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => Ok(InitSession { finished: false }),
        Err(INIT_READY) => Err(BeginInitError::AlreadyInitialized),
        Err(_) => Err(BeginInitError::Busy),
    }
}

pub fn reset_after_shutdown() {
    INIT_STATE.store(INIT_UNINIT, Ordering::Release);
}

pub fn meta() -> &'static Meta {
    unsafe { &*META.0.get() }
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
        self.own_signal.load(Ordering::Relaxed)
    }

    pub(super) fn set_app_handler_present(&self) {
        self.app_handler_present.store(true, Ordering::Relaxed);
    }

    pub(super) fn app_handler_present(&self) -> bool {
        self.app_handler_present.load(Ordering::Relaxed)
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

/// Runtime settings copied from the caller's init config.
///
/// These are written before [`HANDLERS_ENABLED`] is published and are read from the crash path
/// after that publication has been observed.
pub struct Settings {
    pub only_bootstrap: AtomicBool,
    pub debug_log: AtomicBool,
    pub create_alt_stack: AtomicBool,
    pub use_alt_stack: AtomicBool,
    pub block_signals: AtomicBool,
    pub disarm_on_entry: AtomicBool,
    pub close_fds_on_receiver: AtomicBool,
    pub report_fd: AtomicI32,
    pub collector_reap_ms: AtomicI32,
    pub receiver_reap_ms: AtomicI32,
    pub max_frames: AtomicUsize,
}

impl Settings {
    const fn new() -> Self {
        Self {
            only_bootstrap: AtomicBool::new(false),
            debug_log: AtomicBool::new(false),
            create_alt_stack: AtomicBool::new(false),
            use_alt_stack: AtomicBool::new(false),
            block_signals: AtomicBool::new(true),
            disarm_on_entry: AtomicBool::new(false),
            close_fds_on_receiver: AtomicBool::new(true),
            report_fd: AtomicI32::new(-1),
            collector_reap_ms: AtomicI32::new(config::COLLECTOR_REAP_MS_DEFAULT),
            receiver_reap_ms: AtomicI32::new(config::RECEIVER_REAP_MS_DEFAULT),
            max_frames: AtomicUsize::new(config::BACKTRACE_LEVELS_DEFAULT),
        }
    }
}

pub static SETTINGS: Settings = Settings::new();

pub fn clear_signal_state() {
    for slot in &SIGNAL_SLOTS {
        slot.clear();
    }
}

pub fn owns_signal(sig: i32) -> bool {
    sig_index(sig).is_some_and(|i| signal_slot(i).owns_signal())
}

pub fn owned_signal_count() -> u32 {
    SIGNAL_SLOTS
        .iter()
        .filter(|slot| slot.owns_signal())
        .count() as u32
}

pub fn app_handler_present(sig: i32) -> bool {
    sig_index(sig).is_some_and(|i| signal_slot(i).app_handler_present())
}
