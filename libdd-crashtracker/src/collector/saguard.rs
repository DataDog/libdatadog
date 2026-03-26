// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler};

// Provides a lexically-scoped guard for signals
// During execution of the signal handler, it cannot be guaranteed that the signal is handled
// without SA_NODEFER, thus it also cannot be guaranteed that signals like SIGCHLD and SIGPIPE will
// _not_ be emitted during this handler as a result of the handler itself. At the same time, it
// isn't known whether it is safe to merely block all signals, as the user's own handler will be
// given the chance to execute after ours. Thus, we need to prevent the emission of signals we
// might create (and cannot be created during a signal handler except by our own execution) and
// defer any other signals.
// To put it another way, it is conceivable that the crash handling code will emit SIGCHLD or
// SIGPIPE, and instead of risking responding to those signals, it needs to suppress them. On the
// other hand, it can't just "block" (`sigprocmask()`) those signals because this will only defer
// them to the next handler.
pub struct SaGuard<const N: usize> {
    old_sigactions: [(signal::Signal, signal::SigAction); N],
    old_sigmask: signal::SigSet,
}

impl<const N: usize> SaGuard<N> {
    pub fn new(signals: &[signal::Signal; N]) -> anyhow::Result<Self> {
        // Create an empty signal set for suppressing signals
        let mut suppressed_signals = signal::SigSet::empty();
        for signal in signals {
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
        let mut old_sigactions = [(
            signal::Signal::SIGINT,
            SigAction::new(
                SigHandler::SigDfl,
                SaFlags::empty(),
                signal::SigSet::empty(),
            ),
        ); N];

        // Set SIG_IGN for the specified signals and save old handlers
        for (i, &signal) in signals.iter().enumerate() {
            let old_sigaction = unsafe {
                signal::sigaction(
                    signal,
                    &SigAction::new(
                        SigHandler::SigIgn,
                        SaFlags::empty(),
                        signal::SigSet::empty(),
                    ),
                )?
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
        // Restore the original signal actions
        for &(signal, old_sigaction) in &self.old_sigactions {
            unsafe {
                let _ = signal::sigaction(signal, &old_sigaction);
            }
        }

        // Restore the original signal mask
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

    #[test]
    #[cfg_attr(miri, ignore)]
    fn signal_is_ignored_while_guard_is_active() {
        let _guard = SaGuard::<1>::new(&[Signal::SIGUSR1]).unwrap();

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
            let _guard = SaGuard::<1>::new(&[Signal::SIGUSR2]).unwrap();
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
        let _guard = SaGuard::<2>::new(&[Signal::SIGUSR1, Signal::SIGUSR2]).unwrap();

        // Both signals should be safely ignored
        signal::kill(Pid::this(), Signal::SIGUSR1).unwrap();
        signal::kill(Pid::this(), Signal::SIGUSR2).unwrap();
    }
}
