// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.
#![cfg(unix)]

use crate::{
    counters::reset_counters,
    crash_handler::{ensure_receiver, register_crash_handlers},
    crash_handler::{restore_old_handlers, shutdown_receiver, update_receiver_after_fork},
    update_config, update_metadata,
};
use ddcommon::tag::Tag;
use ddcommon::Endpoint;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrashtrackerMetadata {
    pub profiling_library_name: String,
    pub profiling_library_version: String,
    pub family: String,
    // Should include "service", "environment", etc
    pub tags: Vec<Tag>,
}

impl CrashtrackerMetadata {
    pub fn new(
        profiling_library_name: String,
        profiling_library_version: String,
        family: String,
        tags: Vec<Tag>,
    ) -> Self {
        Self {
            profiling_library_name,
            profiling_library_version,
            family,
            tags,
        }
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrashtrackerResolveFrames {
    Never,
    /// Resolving frames in process is experimental, and can fail/crash
    ExperimentalInProcess,
    InReceiver,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrashtrackerConfiguration {
    pub collect_stacktrace: bool,
    pub create_alt_stack: bool,
    pub endpoint: Option<Endpoint>,
    pub path_to_receiver_binary: String,
    pub resolve_frames: CrashtrackerResolveFrames,
    pub stderr_filename: Option<String>,
    pub stdout_filename: Option<String>,
}

impl CrashtrackerConfiguration {
    pub fn new(
        collect_stacktrace: bool,
        create_alt_stack: bool,
        endpoint: Option<Endpoint>,
        path_to_receiver_binary: String,
        resolve_frames: CrashtrackerResolveFrames,
        stderr_filename: Option<String>,
        stdout_filename: Option<String>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !path_to_receiver_binary.is_empty(),
            "Expected a receiver binary"
        );
        anyhow::ensure!(stderr_filename.is_none() && stdout_filename.is_none() || stderr_filename != stdout_filename,
        "Can't give the same filename for stderr and stdout, they will conflict with each other"
    );
        Ok(Self {
            collect_stacktrace,
            create_alt_stack,
            endpoint,
            path_to_receiver_binary,
            resolve_frames,
            stderr_filename,
            stdout_filename,
        })
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
    restore_old_handlers(false)?;
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
pub fn on_fork(
    config: CrashtrackerConfiguration,
    metadata: CrashtrackerMetadata,
) -> anyhow::Result<()> {
    reset_counters()?;
    // Leave the old signal handler in place: they are unaffected by fork.
    // https://man7.org/linux/man-pages/man2/sigaction.2.html
    // The altstack (if any) is similarly unaffected by fork:
    // https://man7.org/linux/man-pages/man2/sigaltstack.2.html

    update_metadata(metadata)?;
    update_config(config.clone())?;

    // See function level comment about why we do this.
    update_receiver_after_fork(config)?;
    Ok(())
}

/// Initialize the crash-tracking infrastructure.
///
/// PRECONDITIONS:
///     None.
/// SAFETY:
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
pub fn init(
    config: CrashtrackerConfiguration,
    metadata: CrashtrackerMetadata,
) -> anyhow::Result<()> {
    // Setup the receiver first, so that if there is a crash detected it has
    // somewhere to go.
    let create_alt_stack = config.create_alt_stack;
    update_metadata(metadata)?;
    update_config(config.clone())?;
    ensure_receiver(config)?;
    register_crash_handlers(create_alt_stack)?;
    Ok(())
}

#[ignore]
#[allow(dead_code)]
// Ignored tests are still run in CI.
// To test, uncomment the line below than run manually
// We can't run this in the main test runner because it (deliberately) crashes,
// and would make all following tests unrunable.
// To run this test,
// ./build-profiling-ffi /tmp/libdatadog
// mkdir /tmp/crashreports
// look in /tmp/crashreports for the crash reports and output files
// Commented out since `ignore` doesn't work in CI.
//#[test]
fn test_crash() {
    use crate::begin_profiling_op;
    use chrono::Utc;
    use ddcommon::parse_uri;

    let time = Utc::now().to_rfc3339();
    let dir = "/tmp/crashreports/";
    let output_url = format!("file://{dir}{time}.txt");

    let endpoint = Some(Endpoint {
        url: parse_uri(&output_url).unwrap(),
        api_key: None,
    });

    let collect_stacktrace = true;
    let path_to_receiver_binary =
        "/tmp/libdatadog/bin/libdatadog-crashtracking-receiver".to_string();
    let create_alt_stack = true;
    let resolve_frames = CrashtrackerResolveFrames::InReceiver;
    let stderr_filename = Some(format!("{dir}/stderr_{time}.txt"));
    let stdout_filename = Some(format!("{dir}/stdout_{time}.txt"));

    let config = CrashtrackerConfiguration::new(
        collect_stacktrace,
        create_alt_stack,
        endpoint,
        path_to_receiver_binary,
        resolve_frames,
        stderr_filename,
        stdout_filename,
    )
    .expect("not to fail");
    let metadata = CrashtrackerMetadata::new(
        "libname".to_string(),
        "version".to_string(),
        "family".to_string(),
        vec![],
    );
    init(config, metadata).expect("not to fail");
    begin_profiling_op(crate::ProfilingOpTypes::CollectingSample).expect("Not to fail");

    let tag = Tag::new("apple", "banana").expect("tag");
    let metadata2 = CrashtrackerMetadata::new(
        "libname".to_string(),
        "version".to_string(),
        "family".to_string(),
        vec![tag],
    );
    update_metadata(metadata2).expect("metadata");

    let p: *const u32 = std::ptr::null();
    let q = unsafe { *p };
    assert_eq!(q, 3);
}
