// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]

use crate::{
    configuration::CrashtrackerReceiverConfig,
    counters::reset_counters,
    crash_handler::{
        ensure_receiver, register_crash_handlers, restore_old_handlers, shutdown_receiver,
        update_receiver_after_fork,
    },
    crash_info::CrashtrackerMetadata,
    update_config, update_metadata, CrashtrackerConfiguration,
};

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
    receiver_config: CrashtrackerReceiverConfig,
    metadata: CrashtrackerMetadata,
) -> anyhow::Result<()> {
    reset_counters()?;
    // Leave the old signal handler in place: they are unaffected by fork.
    // https://man7.org/linux/man-pages/man2/sigaction.2.html
    // The altstack (if any) is similarly unaffected by fork:
    // https://man7.org/linux/man-pages/man2/sigaltstack.2.html

    update_metadata(metadata)?;
    update_config(config)?;

    // See function level comment about why we do this.
    update_receiver_after_fork(&receiver_config)?;
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
    receiver_config: Option<CrashtrackerReceiverConfig>,
    metadata: CrashtrackerMetadata,
) -> anyhow::Result<()> {
    // Setup the receiver first, so that if there is a crash detected it has
    // somewhere to go.
    let create_alt_stack = config.create_alt_stack;
    update_metadata(metadata)?;
    update_config(config)?;
    if let Some(receiver_config) = &receiver_config {
        ensure_receiver(receiver_config)?;
    }
    register_crash_handlers(create_alt_stack)?;
    Ok(())
}

// We can't run this in the main test runner because it (deliberately) crashes,
// and would make all following tests unrunable.
// To run this test,
// ./build-profiling-ffi /tmp/libdatadog
// mkdir /tmp/crashreports
// look in /tmp/crashreports for the crash reports and output files
#[ignore]
#[test]
fn test_crash() {
    use crate::{begin_profiling_op, StacktraceCollection};
    use chrono::Utc;
    use ddcommon::parse_uri;
    use ddcommon::tag::Tag;
    use ddcommon::Endpoint;
    use std::time::Duration;

    let time = Utc::now().to_rfc3339();
    let dir = "/tmp/crashreports/";
    let output_url = format!("file://{dir}{time}.txt");

    let endpoint = Some(Endpoint {
        url: parse_uri(&output_url).unwrap(),
        api_key: None,
    });

    let path_to_receiver_binary =
        "/tmp/libdatadog/bin/libdatadog-crashtracking-receiver".to_string();
    let create_alt_stack = true;
    let resolve_frames = StacktraceCollection::Enabled;
    let stderr_filename = Some(format!("{dir}/stderr_{time}.txt"));
    let stdout_filename = Some(format!("{dir}/stdout_{time}.txt"));
    let timeout = Duration::from_secs(30);
    let receiver_config = Some(
        CrashtrackerReceiverConfig::new(
            vec![],
            vec![],
            path_to_receiver_binary,
            stderr_filename,
            stdout_filename,
        )
        .expect("Not to fail"),
    );
    let config =
        CrashtrackerConfiguration::new(vec![], create_alt_stack, endpoint, resolve_frames, timeout)
            .expect("not to fail");
    let metadata = CrashtrackerMetadata::new(
        "libname".to_string(),
        "version".to_string(),
        "family".to_string(),
        vec![],
    );
    init(config, receiver_config, metadata).expect("not to fail");
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
