// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Public lifecycle API and alternate-signal-stack installation.

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::ptr::null_mut;
use core::sync::atomic::Ordering;

use super::crash;
use super::crash_debug;
use super::sigaction::{install_all_handlers, uninstall_all_handlers};
use crate::signal_owner::{self, SignalOwner};

use crate::collector_signal_safe::config::{self, InitConfig, PrepareError};
use crate::collector_signal_safe::state::{self, BeginInitError};
use crate::collector_signal_safe::{capabilities, sys};

const ALT_STACK_SIZE: usize = 64 * 1024;
const ALT_STACK_GUARD_SIZE: usize = 4096;

const _: () = assert!(crate::collector_signal_safe::SECTION_BUF_CAPACITY <= ALT_STACK_SIZE / 8);

#[repr(C, align(4096))]
struct AltStackLayout {
    guard: [u8; ALT_STACK_GUARD_SIZE],
    usable: [u8; ALT_STACK_SIZE],
}

struct AltStackStorage(UnsafeCell<AltStackLayout>);

unsafe impl Sync for AltStackStorage {}

static ALT_STACK: AltStackStorage = AltStackStorage(UnsafeCell::new(AltStackLayout {
    guard: [0; ALT_STACK_GUARD_SIZE],
    usable: [0; ALT_STACK_SIZE],
}));

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitResult {
    /// Handlers were installed and crash collection is enabled.
    Enabled,
    /// Initialization failed for a reason that cannot be represented more specifically.
    Failed,
    /// The signal-safe collector is already initialized.
    AlreadyInitialized,
    /// Another crash collector already owns process crash signals.
    OwnerConflict,
    /// The supplied init configuration is invalid.
    InvalidConfig,
}

pub fn init(config: &InitConfig<'_>) -> InitResult {
    init_with_prepare(|session| config::apply(config, session.meta_mut()))
}

fn init_with_prepare(
    prepare: impl FnOnce(&mut state::InitSession) -> Result<(), PrepareError>,
) -> InitResult {
    let mut session = match state::begin_init() {
        Ok(session) => session,
        Err(err) => return err.into(),
    };

    if !signal_owner::acquire(SignalOwner::SignalSafeCollector) {
        return InitResult::OwnerConflict;
    }

    let prepared = (|| {
        prepare(&mut session).map_err(InitResult::from)?;
        if !install_alt_stack_if_requested() {
            return Err(InitResult::Failed);
        }
        Ok(())
    })();
    if let Err(err) = prepared {
        signal_owner::release(SignalOwner::SignalSafeCollector);
        return err;
    }

    install_all_handlers();
    session.finish();
    state::HANDLERS_ENABLED.store(true, Ordering::Release);
    InitResult::Enabled
}

/// Complete bootstrap-only mode.
///
/// This is a no-op in normal mode. When `only_bootstrap` was set at init time, this performs a full
/// [`shutdown`] so the collector can validate setup without staying installed for later crashes.
pub fn bootstrap_complete() {
    if state::SETTINGS.only_bootstrap.load(Ordering::Relaxed) {
        shutdown();
    }
}

pub fn shutdown() {
    state::HANDLERS_ENABLED.store(false, Ordering::Release);
    uninstall_all_handlers();
    crash::reset_collecting();
    signal_owner::release(SignalOwner::SignalSafeCollector);
    state::reset_after_shutdown();
}

impl From<BeginInitError> for InitResult {
    fn from(err: BeginInitError) -> Self {
        match err {
            BeginInitError::AlreadyInitialized => Self::AlreadyInitialized,
            BeginInitError::Busy => Self::Failed,
        }
    }
}

impl From<PrepareError> for InitResult {
    fn from(err: PrepareError) -> Self {
        match err {
            PrepareError::InvalidConfig => Self::InvalidConfig,
            PrepareError::Failed => Self::Failed,
        }
    }
}

fn install_alt_stack_if_requested() -> bool {
    if !state::SETTINGS.create_alt_stack.load(Ordering::Relaxed) {
        return true;
    }

    install_alt_stack_with(sys::mprotect_none, install_sigaltstack)
}

fn install_alt_stack_with(
    mprotect_none: fn(*mut u8, usize) -> bool,
    sigaltstack: fn(&libc::stack_t) -> bool,
) -> bool {
    let layout = ALT_STACK.0.get();
    let guard = unsafe { core::ptr::addr_of_mut!((*layout).guard).cast::<u8>() };
    let usable = unsafe { core::ptr::addr_of_mut!((*layout).usable).cast::<c_void>() };
    if !mprotect_none(guard, ALT_STACK_GUARD_SIZE) {
        capabilities::note_degraded(capabilities::DEGRADED_ALT_STACK_GUARD_UNAVAILABLE);
        crash_debug(b"alt stack guard unavailable", -1);
    }

    let stack = libc::stack_t {
        ss_sp: usable,
        ss_flags: 0,
        ss_size: ALT_STACK_SIZE,
    };
    sigaltstack(&stack)
}

fn install_sigaltstack(stack: &libc::stack_t) -> bool {
    unsafe { libc::sigaltstack(stack, null_mut()) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_stack_guard_failure_is_degraded_but_not_fatal() {
        let _guard = crate::collector_signal_safe::TEST_GLOBAL_LOCK
            .lock()
            .expect("test lock poisoned");

        capabilities::publish(b"/definitely/missing-signal-safe-receiver\0", -1, false);
        assert!(install_alt_stack_with(|_, _| false, |_| true));
        assert!(capabilities::degradations()
            .contains(capabilities::DEGRADED_ALT_STACK_GUARD_UNAVAILABLE));
    }

    #[cfg(not(feature = "collector"))]
    #[test]
    fn lifecycle_can_install_and_shutdown() {
        let _guard = crate::collector_signal_safe::TEST_GLOBAL_LOCK
            .lock()
            .expect("test lock poisoned");

        let config = InitConfig {
            receiver_path: b"/bin/cat",
            ..InitConfig::default()
        };
        assert_eq!(init(&config), InitResult::Enabled);
        assert!(state::HANDLERS_ENABLED.load(Ordering::Acquire));
        assert_eq!(init(&config), InitResult::AlreadyInitialized);
        shutdown();
        assert!(!state::HANDLERS_ENABLED.load(Ordering::Acquire));
        assert_eq!(init(&config), InitResult::Enabled);
        assert!(state::HANDLERS_ENABLED.load(Ordering::Acquire));
        shutdown();
        assert!(!state::HANDLERS_ENABLED.load(Ordering::Acquire));
    }
}
