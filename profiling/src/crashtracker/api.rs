// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::{
    counters::reset_counters,
    crash_handler::{
        register_crash_handlers, replace_receiver, restore_old_handlers, setup_receiver,
        shutdown_receiver,
    },
};
use ddcommon::tag::Tag;
use ddcommon::Endpoint;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub profiling_library_name: String,
    pub profiling_library_version: String,
    pub family: String,
    pub tags: Option<Vec<Tag>>,
}

impl Metadata {
    pub fn new(
        profiling_library_name: String,
        profiling_library_version: String,
        family: String,
        tags: Option<Vec<Tag>>,
    ) -> Self {
        Self {
            profiling_library_name,
            profiling_library_version,
            family,
            tags,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Configuration {
    pub create_alt_stack: bool,
    pub endpoint: Option<Endpoint>,
    pub output_filename: Option<String>,
    pub path_to_receiver_binary: String,
    pub resolve_frames: bool,
}

impl Configuration {
    pub fn new(
        create_alt_stack: bool,
        endpoint: Option<Endpoint>,
        output_filename: Option<String>,
        path_to_receiver_binary: String,
        resolve_frames: bool,
    ) -> Self {
        Self {
            create_alt_stack,
            endpoint,
            output_filename,
            path_to_receiver_binary,
            resolve_frames,
        }
    }
}

/// Cleans up after the crash-tracker:
/// Unregister the crash handler, restore the previous handler (if any), and
/// shut down the receiver.  Note that the use of this function is optional:
/// the receiver will automatically shutdown when the pipe is closed on program
/// exit.
///
/// PRECONDITIONS:
///     This function assumes that the crash-tracker has previously been
///     initialized.
/// SAFETY:
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
pub fn shutdown_crash_handler() -> anyhow::Result<()> {
    restore_old_handlers()?;
    shutdown_receiver()?;
    Ok(())
}

/// Reinitialize the crash-tracking infrastructure after a fork.
/// This should be one of the first things done after a fork, to minimize the
/// chance that a crash occurs between the fork, and this call.
/// In particular, reset the counters that track the profiler state machine,
/// and start a new receiver to collect data from this fork.
/// NOTE: An alternative design would be to have a 1:many sidecar listening on a
/// socket instead of 1:1 receiver listening on a pipe, but the only real
/// advantage would be to have fewer processes in `ps -a`.
///
/// PRECONDITIONS:
///     This function assumes that the crash-tracker has previously been
///     initialized.
/// SAFETY:
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
pub fn on_fork(config: Configuration, metadata: Metadata) -> anyhow::Result<()> {
    reset_counters()?;
    // Leave the old signal handler in place: they are unaffected by fork.
    // https://man7.org/linux/man-pages/man2/sigaction.2.html
    // The altstack (if any) is similarly unaffected by fork:
    // https://man7.org/linux/man-pages/man2/sigaltstack.2.html

    // See function level comment about why we do this.
    replace_receiver(&config, &metadata)?;
    Ok(())
}

/// Initilize the crash-tracking infrasturcture.
///
/// PRECONDITIONS:
///     This function assumes that the crash-tracker is uninitialized
/// SAFETY:
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
pub fn init(config: Configuration, metadata: Metadata) -> anyhow::Result<()> {
    // Setup the receiver first, so that if there is a crash detected it has
    // somewhere to go.
    setup_receiver(&config, &metadata)?;
    register_crash_handlers(&config)?;
    Ok(())
}

#[ignore]
#[test]
fn test_crash() {
    use crate::crashtracker::begin_profiling_op;
    use chrono::Utc;

    let endpoint = None;
    let output_filename = Some(format!("/tmp/crashreports/{}.txt", Utc::now().to_rfc3339()));

    #[cfg(target_os = "macos")]
    let path_to_receiver_binary = "/Users/daniel.schwartznarbonne/go/src/github.com/DataDog/libdatadog/target/debug/profiling-crashtracking-receiver".to_string();
    #[cfg(target_os = "linux")]
    let path_to_receiver_binary =
        "/tmp/libdatadog/debug/profiling-crashtracking-receiver".to_string();
    let create_alt_stack = true;
    let resolve_frames = true;
    let config = Configuration::new(
        create_alt_stack,
        endpoint,
        output_filename,
        path_to_receiver_binary,
        resolve_frames,
    );
    let metadata = Metadata::new(
        "libname".to_string(),
        "version".to_string(),
        "family".to_string(),
        None,
    );
    init(config, metadata).expect("not to fail");
    begin_profiling_op(crate::crashtracker::ProfilingOpTypes::CollectingSample)
        .expect("Not to fail");
    let p: *const u32 = std::ptr::null();
    let q = unsafe { *p };
    assert_eq!(q, 3);
}
