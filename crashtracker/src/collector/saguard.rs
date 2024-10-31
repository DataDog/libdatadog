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
        // Except for SigChld and SigPipe, which are instantiated to SigIgn by default on all
        // (most?) systems, the rest are instantiated to SigDfl.  This section attempts to restore
        // that defaulting behavior.
        // See <https://man7.org/linux/man-pages/man7/signal.7.html> for details on which signals
        // get which default action and what the different defaults mean.  We follow the guidance
        // that SIGPIPE and SIGCHLD and somewhat special.
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
