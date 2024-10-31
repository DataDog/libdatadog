// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]

use crate::{
    clear_spans, clear_traces,
    collector::crash_handler::{configure_receiver, register_crash_handlers, restore_old_handlers},
    crash_info::CrashtrackerMetadata,
    reset_counters,
    shared::configuration::CrashtrackerReceiverConfig,
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
    Ok(())
}

/// Reinitialize the crash-tracking infrastructure after a fork.
/// This should be one of the first things done after a fork, to minimize the
/// chance that a crash occurs between the fork, and this call.
/// In particular, reset the counters that track the profiler state machine.
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
    clear_spans()?;
    clear_traces()?;
    reset_counters()?;
    // Leave the old signal handler in place: they are unaffected by fork.
    // https://man7.org/linux/man-pages/man2/sigaction.2.html
    // The altstack (if any) is similarly unaffected by fork:
    // https://man7.org/linux/man-pages/man2/sigaltstack.2.html

    update_metadata(metadata)?;
    update_config(config)?;
    configure_receiver(receiver_config);
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
    receiver_config: CrashtrackerReceiverConfig,
    metadata: CrashtrackerMetadata,
) -> anyhow::Result<()> {
    // Setup the receiver first, so that if there is a crash detected it has
    // somewhere to go.
    update_metadata(metadata)?;
    update_config(config)?;
    configure_receiver(receiver_config);
    register_crash_handlers()?;
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
fn test_crash() -> anyhow::Result<()> {
    use crate::{begin_op, StacktraceCollection};
    use chrono::Utc;
    use ddcommon::tag;
    use ddcommon::Endpoint;
    use std::time::Duration;

    let time = Utc::now().to_rfc3339();
    let dir = "/tmp/crashreports/";
    let output_url = format!("file://{dir}{time}.txt");

    let endpoint = Some(Endpoint::from_slice(&output_url));

    let path_to_receiver_binary =
        "/tmp/libdatadog/bin/libdatadog-crashtracking-receiver".to_string();
    let create_alt_stack = true;
    let use_alt_stack = true;
    let resolve_frames = StacktraceCollection::EnabledWithInprocessSymbols;
    let stderr_filename = Some(format!("{dir}/stderr_{time}.txt"));
    let stdout_filename = Some(format!("{dir}/stdout_{time}.txt"));
    let timeout_ms = 10_000;
    let receiver_config = CrashtrackerReceiverConfig::new(
        vec![],
        vec![],
        path_to_receiver_binary,
        stderr_filename,
        stdout_filename,
    )?;
    let config = CrashtrackerConfiguration::new(
        vec![],
        create_alt_stack,
        use_alt_stack,
        endpoint,
        resolve_frames,
        timeout_ms,
    )?;
    let metadata = CrashtrackerMetadata::new(
        "libname".to_string(),
        "version".to_string(),
        "family".to_string(),
        vec![],
    );
    init(config, receiver_config, metadata)?;
    begin_op(crate::OpTypes::ProfilerCollectingSample)?;
    super::insert_span(42)?;
    super::insert_trace(u128::MAX)?;
    super::insert_span(12)?;
    super::insert_trace(99399939399939393993)?;

    let tag = tag!("apple", "banana");
    let metadata2 = CrashtrackerMetadata::new(
        "libname".to_string(),
        "version".to_string(),
        "family".to_string(),
        vec![tag],
    );
    update_metadata(metadata2).expect("metadata");

    std::thread::sleep(Duration::from_secs(2));

    let p: *const u32 = std::ptr::null();
    let q = unsafe { *p };
    assert_eq!(q, 3);
    Ok(())
}
