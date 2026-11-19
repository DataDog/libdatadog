// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The open files limit of the sidecar.

use std::io;
#[cfg(target_os = "linux")]
use tracing::{info, warn};

/// Below this soft limit a sidecar shared by a few hundred workers can run out of descriptors:
/// 1024 did, with 160 PHP-FPM workers and a slow agent.
#[cfg(target_os = "linux")]
const LOW_SOFT_LIMIT: libc::rlim_t = 4096;

fn open_files_limit() -> io::Result<libc::rlimit> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit only writes `limit`.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
        Ok(limit)
    } else {
        Err(io::Error::last_os_error())
    }
}

fn display(limit: libc::rlim_t) -> String {
    if limit == libc::RLIM_INFINITY {
        "unlimited".to_owned()
    } else {
        limit.to_string()
    }
}

/// The current limits, as logged.
pub(crate) fn describe_limits() -> String {
    match open_files_limit() {
        Ok(limit) => format!(
            "RLIMIT_NOFILE soft={} hard={}",
            display(limit.rlim_cur),
            display(limit.rlim_max)
        ),
        Err(e) => format!("RLIMIT_NOFILE unavailable ({e})"),
    }
}

/// The sidecar serves every client, yet inherits the soft limit of the one that spawned it,
/// often 1024. Only the hard limit is meant as a ceiling.
#[cfg(target_os = "linux")]
pub(crate) fn raise_open_files_limit() {
    let mut limit = match open_files_limit() {
        Ok(limit) => limit,
        Err(e) => {
            warn!("Could not read RLIMIT_NOFILE: {e}");
            return;
        }
    };
    info!("At startup, {}", describe_limits());
    if limit.rlim_cur < limit.rlim_max {
        let soft = limit.rlim_cur;
        limit.rlim_cur = limit.rlim_max;
        // SAFETY: setrlimit only reads `limit`.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } == 0 {
            info!(
                "Raised the RLIMIT_NOFILE soft limit from {soft} to {}",
                display(limit.rlim_max)
            );
        } else {
            warn!(
                "Could not raise the RLIMIT_NOFILE soft limit from {soft} to {}: {}",
                display(limit.rlim_max),
                io::Error::last_os_error()
            );
            limit.rlim_cur = soft;
        }
    }
    if limit.rlim_cur < LOW_SOFT_LIMIT {
        warn!(
            "The RLIMIT_NOFILE soft limit is {}, below {LOW_SOFT_LIMIT}: the sidecar may run out \
             of descriptors with many clients. Raise the hard limit of the service that starts \
             PHP, e.g. systemd LimitNOFILE",
            display(limit.rlim_cur)
        );
    }
}
