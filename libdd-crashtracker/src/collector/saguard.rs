// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler};

// Provides a lexically-scoped guard for signal suppression.
//
// During crash handling we may generate signals such as SIGPIPE (pipe writes) and SIGCHLD
// (fork/exec child lifecycle). We want to prevent re-entrant handling while preserving process
// semantics needed by cleanup code.
//
// This guard supports per-signal policy:
// - IgnoreAndBlock: block delivery and temporarily set disposition to SIG_IGN.
// - BlockOnly: block delivery while leaving disposition unchanged.
//
// In practice, SIGPIPE is usually IgnoreAndBlock, while SIGCHLD should usually be BlockOnly
// because SIG_IGN for SIGCHLD can change child-reaping semantics (waitpid/ECHILD behavior).
pub struct SaGuard<const N: usize> {
    old_sigactions: [(signal::Signal, Option<signal::SigAction>); N],
    old_sigmask: signal::SigSet,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SuppressionMode {
    /// Block delivery and set disposition to SIG_IGN while the guard is active.
    IgnoreAndBlock,
    /// Only block delivery while the guard is active
    BlockOnly,
}

impl<const N: usize> SaGuard<N> {
    pub fn new_with_modes(
        signals: &[(signal::Signal, SuppressionMode); N],
    ) -> anyhow::Result<Self> {
        // Create an empty signal set for suppressing signals
        let mut suppressed_signals = signal::SigSet::empty();
        for (signal, _) in signals {
            suppressed_signals.add(*signal);
        }

        // Save the current signal mask and block all signals except the suppressed ones
        let mut old_sigmask = signal::SigSet::empty();
        signal::sigprocmask(
            signal::SigmaskHow::SIG_BLOCK,
            Some(&suppressed_signals),
            Some(&mut old_sigmask),
        )?;

        // Initialize array for saving old signal actions
        let mut old_sigactions = [(signal::Signal::SIGINT, None); N];

        // Set SIG_IGN for configured signals and save old handlers when disposition changes
        for (i, &(signal, mode)) in signals.iter().enumerate() {
            let old_sigaction = match mode {
                SuppressionMode::IgnoreAndBlock => Some(unsafe {
                    signal::sigaction(
                        signal,
                        &SigAction::new(
                            SigHandler::SigIgn,
                            SaFlags::empty(),
                            signal::SigSet::empty(),
                        ),
                    )?
                }),
                SuppressionMode::BlockOnly => None,
            };
            old_sigactions[i] = (signal, old_sigaction);
        }

        Ok(Self {
            old_sigactions,
            old_sigmask,
        })
    }
}

impl<const N: usize> Drop for SaGuard<N> {
    fn drop(&mut self) {
        // Restore the original signal actions first, before unblocking signals.
        // This prevents a window where deferred signals could fire with the wrong handler.
        for &(signal, old_sigaction) in &self.old_sigactions {
            if let Some(old_sigaction) = old_sigaction {
                unsafe {
                    let _ = signal::sigaction(signal, &old_sigaction);
                }
            }
        }

        // Now restore the original signal mask, which will deliver any deferred signals
        let _ = signal::sigprocmask(
            signal::SigmaskHow::SIG_SETMASK,
            Some(&self.old_sigmask),
            None,
        );
    }
}

#[cfg(test)]
mod single_threaded_tests {
    use super::*;
    use nix::sys::signal::{self, Signal};
    use nix::unistd::Pid;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    // These tests mutate global signal state, so we need to lock to avoid race conditions
    // even in single-threaded mode, as signal state can persist between test runs
    static SIGNAL_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    #[cfg_attr(miri, ignore)]
    fn signal_is_ignored_while_guard_is_active() {
        let _test_lock = SIGNAL_TEST_LOCK.lock().unwrap();
        let _guard =
            SaGuard::<1>::new_with_modes(&[(Signal::SIGUSR1, SuppressionMode::IgnoreAndBlock)])
                .unwrap();

        // Send SIGUSR1 to the process. The default action is to terminate, so if
        // the guard didn't set SIG_IGN this test process would die
        signal::kill(Pid::this(), Signal::SIGUSR1).unwrap();
    }

    /// After the guard is dropped, the original handler should be restored.
    /// Install a custom handler, create a guard,drop the guard, then send the
    /// signal and verify the custom handler fires
    #[test]
    #[cfg_attr(miri, ignore)]
    fn original_handler_restored_after_drop() {
        let _test_lock = SIGNAL_TEST_LOCK.lock().unwrap();
        static HANDLER_CALLED: AtomicBool = AtomicBool::new(false);

        extern "C" fn custom_handler(_: libc::c_int) {
            HANDLER_CALLED.store(true, Ordering::SeqCst);
        }

        // Install a custom handler
        let custom_action = SigAction::new(
            SigHandler::Handler(custom_handler),
            SaFlags::empty(),
            signal::SigSet::empty(),
        );
        let prev = unsafe { signal::sigaction(Signal::SIGUSR2, &custom_action).unwrap() };

        // Create then drop the guard (dropped when out of scope)
        {
            let _guard =
                SaGuard::<1>::new_with_modes(&[(Signal::SIGUSR2, SuppressionMode::IgnoreAndBlock)])
                    .unwrap();
            signal::kill(Pid::this(), Signal::SIGUSR2).unwrap();
            assert!(
                !HANDLER_CALLED.load(Ordering::SeqCst),
                "custom handler should not fire while guard is active"
            );
        }
        // Guard is dropped; custom handler should be restored
        HANDLER_CALLED.store(false, Ordering::SeqCst);
        unsafe {
            libc::raise(Signal::SIGUSR2 as libc::c_int);
        }
        assert!(
            HANDLER_CALLED.load(Ordering::SeqCst),
            "custom handler should fire after guard is dropped"
        );

        // Restore original handler
        unsafe {
            signal::sigaction(Signal::SIGUSR2, &prev).unwrap();
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn multiple_signals_ignored() {
        let _test_lock = SIGNAL_TEST_LOCK.lock().unwrap();
        let _guard = SaGuard::<2>::new_with_modes(&[
            (Signal::SIGUSR1, SuppressionMode::IgnoreAndBlock),
            (Signal::SIGUSR2, SuppressionMode::IgnoreAndBlock),
        ])
        .unwrap();

        // Both signals should be safely ignored
        signal::kill(Pid::this(), Signal::SIGUSR1).unwrap();
        signal::kill(Pid::this(), Signal::SIGUSR2).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn block_only_defers_signal_delivery() -> anyhow::Result<()> {
        let _test_lock = SIGNAL_TEST_LOCK.lock().unwrap();
        static SIGUSR1_COUNT: AtomicBool = AtomicBool::new(false);

        extern "C" fn sigusr1_handler(_: libc::c_int) {
            SIGUSR1_COUNT.store(true, Ordering::SeqCst);
        }

        let sig = Signal::SIGUSR1;

        // Install a known handler and save the previous one so we can restore it
        let old_action = unsafe {
            signal::sigaction(
                sig,
                &SigAction::new(
                    SigHandler::Handler(sigusr1_handler),
                    SaFlags::empty(),
                    signal::SigSet::empty(),
                ),
            )?
        };

        // Reset handler state
        SIGUSR1_COUNT.store(false, Ordering::SeqCst);

        {
            let _guard = SaGuard::<1>::new_with_modes(&[(sig, SuppressionMode::BlockOnly)])?;

            // Send SIGUSR1 to ourselves while it is blocked
            signal::raise(sig)?;

            // Because the signal is blocked, the handler should not have run yet
            assert!(
                !SIGUSR1_COUNT.load(Ordering::SeqCst),
                "Handler should not be called while signal is blocked by BlockOnly guard"
            );
        } // guard drops here; old mask is restored, SIGUSR1 should now be delivered
          // After unblocking, the signal should be handled
        assert!(
            SIGUSR1_COUNT.load(Ordering::SeqCst),
            "Handler should be called after BlockOnly guard drops and pending signal is delivered"
        );
        // Restore the prev disposition
        unsafe {
            signal::sigaction(sig, &old_action)?;
        }

        Ok(())
    }
}
