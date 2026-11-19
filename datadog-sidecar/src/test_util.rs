// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Helpers for tests that need a process of their own.

use std::process::{Command, Stdio};

/// The environment variable telling a test that it runs as the process another test started.
pub(crate) const RERUN: &str = "_DD_TEST_RERUN";

/// Whether this process is the re-run of `test` started by [`rerun`].
pub(crate) fn is_rerun(test: &str) -> bool {
    std::env::var(RERUN).as_deref() == Ok(test)
}

/// Re-runs `test` (its path in the crate, e.g. `unix::tests::x`) in a process of its own, where
/// [`is_rerun`] holds. Process-wide state, like resource limits, stays out of concurrent tests.
pub(crate) fn rerun(test: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test, "--nocapture"])
        .env(RERUN, test)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Sets the soft RLIMIT_NOFILE of this process, returning the previous one.
pub(crate) fn set_soft_nofile(soft: libc::rlim_t) -> libc::rlim_t {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit and setrlimit only read and write `limit`.
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit), 0);
        let previous = limit.rlim_cur;
        limit.rlim_cur = soft;
        assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
        previous
    }
}
