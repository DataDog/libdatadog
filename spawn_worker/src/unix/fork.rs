// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use nix::libc;

#[cfg(target_os = "linux")]
use linux_raw_sys::general::{kernel_sigaction, kernel_sigset_t};

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("spawn_worker process creation is supported only on Linux and macOS");

#[cfg(target_os = "linux")]
type SignalMask = kernel_sigset_t;
#[cfg(target_os = "macos")]
type SignalMask = libc::sigset_t;

pub(crate) enum RawFork {
    Parent(libc::pid_t),
    Child,
    Error(libc::c_int),
}

pub(crate) enum KeepChildSignalsBlocked {
    Yes,
    No,
}

/// Sets test panic handler that will ensure exit(1) is called after
/// the original panic handler
pub fn set_default_child_panic_handler() {
    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |p| {
        old_hook(p);
        std::process::exit(1);
    }));
}

/// Call _Fork or a substitute thereof.
pub(crate) unsafe fn fork_skip_atfork_handlers(
    keep_child_signals_blocked: KeepChildSignalsBlocked,
) -> RawFork {
    unsafe extern "C" {
        fn ddog_spawn_worker_fork(error: *mut libc::c_int) -> libc::pid_t;
    }

    let old_mask = match unsafe { block_all_signals() } {
        Ok(mask) => mask,
        Err(error) => return RawFork::Error(error),
    };

    // Block outside `_Fork`: glibc 2.34-2.40 does not protect its
    // child-side bookkeeping from inherited signal handlers.
    let mut fork_error = 0;
    let result = unsafe { ddog_spawn_worker_fork(&mut fork_error) };

    let restore_result =
        if result == 0 && matches!(keep_child_signals_blocked, KeepChildSignalsBlocked::Yes) {
            Ok(())
        } else {
            unsafe { set_signal_mask(&old_mask) }
        };

    if result == -1 {
        return RawFork::Error(fork_error);
    }
    if let Err(error) = restore_result {
        if result == 0 {
            unsafe {
                libc::_exit(126);
            }
        }
        return RawFork::Error(error);
    }

    match result {
        0 => RawFork::Child,
        pid => RawFork::Parent(pid),
    }
}

/// Reset every mutable signal disposition to `SIG_DFL`. Call only in a child
/// whose signals remain blocked after process creation.
pub(crate) unsafe fn reset_signal_dispositions() -> Result<(), libc::c_int> {
    #[cfg(target_os = "linux")]
    {
        // Do not use libc::sigaction here. In particular, musl takes its
        // __abort_lock for SIGABRT, which the raw-fork fallback cannot repair.
        let action = kernel_sigaction {
            sa_handler_kernel: None,
            sa_flags: 0,
            sa_restorer: None,
            sa_mask: unsafe { std::mem::zeroed() },
        };
        let last_signal = match libc::c_int::try_from(linux_raw_sys::general::_NSIG) {
            Ok(signal) => signal,
            Err(_) => return Err(libc::EINVAL),
        };

        let mut signal = 1;
        while signal <= last_signal {
            // These two can't have their dispositions changed
            if signal != libc::SIGKILL && signal != libc::SIGSTOP {
                let result = unsafe {
                    libc::syscall(
                        libc::SYS_rt_sigaction,
                        signal,
                        &action,
                        std::ptr::null_mut::<kernel_sigaction>(),
                        std::mem::size_of::<kernel_sigset_t>(),
                    )
                };
                if result == -1 {
                    return Err(unsafe { last_errno() });
                }
            }
            signal += 1;
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        // Darwin has no rt_sigaction syscall. Supported Darwin releases run
        // LibSystem's internal repair hooks before vfork returns in the child.
        const SIGNAL_COUNT: libc::c_int = 32;

        let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
        action.sa_sigaction = libc::SIG_DFL;
        if unsafe { libc::sigemptyset(&mut action.sa_mask) } == -1 {
            return Err(unsafe { last_errno() });
        }

        let mut signal = 1;
        while signal < SIGNAL_COUNT {
            if signal != libc::SIGKILL
                && signal != libc::SIGSTOP
                && unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } == -1
            {
                return Err(unsafe { last_errno() });
            }
            signal += 1;
        }
        Ok(())
    }
}

/// Install an empty signal mask. Call only in the final child immediately
/// before replacing it with the prepared executable.
pub(crate) unsafe fn clear_signal_mask() -> Result<(), libc::c_int> {
    let no_signals = unsafe { std::mem::zeroed::<SignalMask>() };
    unsafe { set_signal_mask(&no_signals) }
}

#[cfg(target_os = "linux")]
unsafe fn block_all_signals() -> Result<SignalMask, libc::c_int> {
    // Public sigprocmask/pthread_sigmask filters libc-internal signals such as
    // glibc's SIGCANCEL and SIGSETXID, so operate on the exact kernel mask.
    let mut all_signals = unsafe { std::mem::zeroed::<SignalMask>() };
    all_signals.sig.fill(!0);
    let mut old_mask = unsafe { std::mem::zeroed::<SignalMask>() };
    let result = unsafe {
        libc::syscall(
            libc::SYS_rt_sigprocmask,
            libc::SIG_BLOCK,
            &all_signals,
            &mut old_mask,
            std::mem::size_of::<SignalMask>(),
        )
    };
    if result == -1 {
        Err(unsafe { last_errno() })
    } else {
        Ok(old_mask)
    }
}

#[cfg(target_os = "macos")]
unsafe fn block_all_signals() -> Result<SignalMask, libc::c_int> {
    let mut all_signals = unsafe { std::mem::zeroed::<SignalMask>() };
    if unsafe { libc::sigfillset(&mut all_signals) } == -1 {
        return Err(unsafe { last_errno() });
    }
    let mut old_mask = unsafe { std::mem::zeroed::<SignalMask>() };
    if unsafe { libc::sigprocmask(libc::SIG_BLOCK, &all_signals, &mut old_mask) } == -1 {
        Err(unsafe { last_errno() })
    } else {
        Ok(old_mask)
    }
}

#[cfg(target_os = "linux")]
unsafe fn set_signal_mask(mask: &SignalMask) -> Result<(), libc::c_int> {
    let result = unsafe {
        libc::syscall(
            libc::SYS_rt_sigprocmask,
            libc::SIG_SETMASK,
            mask,
            std::ptr::null_mut::<SignalMask>(),
            std::mem::size_of::<SignalMask>(),
        )
    };
    if result == -1 {
        Err(unsafe { last_errno() })
    } else {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
unsafe fn set_signal_mask(mask: &SignalMask) -> Result<(), libc::c_int> {
    if unsafe { libc::sigprocmask(libc::SIG_SETMASK, mask, std::ptr::null_mut()) } == -1 {
        Err(unsafe { last_errno() })
    } else {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
unsafe fn last_errno() -> libc::c_int {
    unsafe { *libc::__errno_location() }
}

#[cfg(target_os = "macos")]
unsafe fn last_errno() -> libc::c_int {
    unsafe { *libc::__error() }
}

#[cfg(test)]
mod single_threaded_tests {
    use std::sync::{atomic::AtomicUsize, Mutex, Once};

    use crate::assert_child_exit;

    static FORK_TEST_LOCK: Mutex<()> = Mutex::new(());
    static ATFORK_CALLS: AtomicUsize = AtomicUsize::new(0);
    static REGISTER_ATFORK: Once = Once::new();

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_fork_skip_atfork_handlers() {
        let _guard = FORK_TEST_LOCK.lock().unwrap();
        REGISTER_ATFORK.call_once(|| unsafe {
            assert_eq!(
                0,
                libc::pthread_atfork(
                    Some(record_atfork_call),
                    Some(record_atfork_call),
                    Some(record_atfork_call),
                )
            );
        });
        ATFORK_CALLS.store(0, std::sync::atomic::Ordering::Relaxed);

        match unsafe { super::fork_skip_atfork_handlers(super::KeepChildSignalsBlocked::No) } {
            super::RawFork::Child => unsafe {
                let exit_code = if ATFORK_CALLS.load(std::sync::atomic::Ordering::Relaxed) == 0 {
                    0
                } else {
                    1
                };
                libc::_exit(exit_code);
            },
            super::RawFork::Parent(pid) => {
                assert_child_exit!(pid);
                assert_eq!(0, ATFORK_CALLS.load(std::sync::atomic::Ordering::Relaxed));
            }
            super::RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_fork_restores_signal_mask_in_parent_and_child() {
        let _guard = FORK_TEST_LOCK.lock().unwrap();
        let mut original = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        assert_eq!(0, unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut original)
        });

        let mut expected = original;
        assert_eq!(0, unsafe { libc::sigaddset(&mut expected, libc::SIGUSR1) });
        assert_eq!(0, unsafe { libc::sigdelset(&mut expected, libc::SIGUSR2) });
        assert_eq!(0, unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &expected, std::ptr::null_mut())
        });

        let forked =
            unsafe { super::fork_skip_atfork_handlers(super::KeepChildSignalsBlocked::No) };
        let mut actual = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        assert_eq!(0, unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut actual)
        });
        let restored = unsafe { libc::sigismember(&actual, libc::SIGUSR1) } == 1
            && unsafe { libc::sigismember(&actual, libc::SIGUSR2) } == 0;

        match forked {
            super::RawFork::Child => unsafe {
                libc::_exit(if restored { 0 } else { 1 });
            },
            super::RawFork::Parent(pid) => {
                assert!(restored);
                assert_eq!(0, unsafe {
                    libc::pthread_sigmask(libc::SIG_SETMASK, &original, std::ptr::null_mut())
                });
                assert_child_exit!(pid);
            }
            super::RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_fork_can_keep_signals_blocked_in_child() {
        let _guard = FORK_TEST_LOCK.lock().unwrap();
        let mut original = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        assert_eq!(0, unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut original)
        });

        let mut expected_parent = original;
        assert_eq!(0, unsafe {
            libc::sigdelset(&mut expected_parent, libc::SIGUSR1)
        });
        assert_eq!(0, unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &expected_parent, std::ptr::null_mut())
        });

        let forked =
            unsafe { super::fork_skip_atfork_handlers(super::KeepChildSignalsBlocked::Yes) };
        let mut actual = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        let mask_read =
            unsafe { libc::sigprocmask(libc::SIG_SETMASK, std::ptr::null(), &mut actual) } == 0;
        let signal_is_blocked =
            mask_read && unsafe { libc::sigismember(&actual, libc::SIGUSR1) } == 1;

        match forked {
            super::RawFork::Child => unsafe {
                libc::_exit(if signal_is_blocked { 0 } else { 1 });
            },
            super::RawFork::Parent(pid) => {
                assert!(!signal_is_blocked);
                assert_eq!(0, unsafe {
                    libc::pthread_sigmask(libc::SIG_SETMASK, &original, std::ptr::null_mut())
                });
                assert_child_exit!(pid);
            }
            super::RawFork::Error(error) => panic!("fork failed with errno {error}"),
        }
    }

    extern "C" fn record_atfork_call() {
        ATFORK_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}
