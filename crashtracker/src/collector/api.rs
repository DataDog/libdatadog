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
    use ddcommon_net1::Endpoint;
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
        None,
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

#[test]
fn test_altstack_paradox() -> anyhow::Result<()> {
    use crate::StacktraceCollection;
    use chrono::Utc;
    use ddcommon_net1::Endpoint;

    let time = Utc::now().to_rfc3339();
    let dir = "/tmp/crashreports/";
    let output_url = format!("file://{dir}{time}.txt");

    let endpoint = Some(Endpoint::from_slice(&output_url));

    let create_alt_stack = true;
    let use_alt_stack = false;
    let resolve_frames = StacktraceCollection::EnabledWithInprocessSymbols;
    let timeout_ms = 10_000;

    // This should return an error, because we're creating an altstack without using it
    let config = CrashtrackerConfiguration::new(
        vec![],
        create_alt_stack,
        use_alt_stack,
        endpoint,
        resolve_frames,
        timeout_ms,
        None,
    );

    // This is slightly over-tuned to the language of the error message, but it'd require some
    // novel engineering just for this test in order to tighten this up.
    let err = config.unwrap_err();
    assert_eq!(
        err.to_string(),
        "Cannot create an altstack without using it"
    );
    Ok(())
}

#[cfg(test)]
#[cfg(target_os = "linux")]
fn get_sigaltstack() -> Option<libc::stack_t> {
    let mut sigaltstack = libc::stack_t {
        ss_sp: std::ptr::null_mut(),
        ss_flags: 0,
        ss_size: 0,
    };
    let res = unsafe { libc::sigaltstack(std::ptr::null(), &mut sigaltstack) };
    if res == 0 {
        Some(sigaltstack)
    } else {
        None
    }
}

#[cfg_attr(miri, ignore)]
#[cfg(target_os = "linux")]
#[test]
fn test_altstack_use_create() -> anyhow::Result<()> {
    // This test initializes crashtracking in a fork, then waits on the exit status of the child.
    // We check for an atypical exit status in order to ensure that only our desired exit path is
    // taken.
    use crate::StacktraceCollection;
    use chrono::Utc;
    use ddcommon_net1::Endpoint;

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
        None,
    )?;
    let metadata = CrashtrackerMetadata::new(
        "libname".to_string(),
        "version".to_string(),
        "family".to_string(),
        vec![],
    );

    // At this point we fork, because we're going to be looking at process-level state
    match unsafe { libc::fork() } {
        -1 => {
            panic!("Failed to fork");
        }
        0 => {
            // Child process
            // Get the current state of the altstack
            let initial_sigaltstack = get_sigaltstack();
            assert!(
                initial_sigaltstack.is_some(),
                "Failed to get initial sigaltstack"
            );

            // Initialize crashtracking.  This will
            // - create a new altstack
            // - set the SIGUBS/SIGSEGV handlers with SA_ONSTACK
            init(config, receiver_config, metadata)?;

            // Get the state of the altstack after initialization
            let after_init_sigaltstack = get_sigaltstack();

            // Compare the initial and after-init sigaltstacks
            if initial_sigaltstack == after_init_sigaltstack {
                eprintln!("Initial sigaltstack: {:?}", initial_sigaltstack);
                std::process::exit(-5);
            }

            // Check the SIGBUS and SIGSEGV handlers are set with SA_ONSTACK
            let mut sigaction = libc::sigaction {
                sa_sigaction: 0,
                sa_mask: unsafe { std::mem::zeroed::<libc::sigset_t>() },
                sa_flags: 0,
                sa_restorer: None,
            };

            // First, SIGBUS
            let res = unsafe { libc::sigaction(libc::SIGBUS, std::ptr::null(), &mut sigaction) };
            if res != 0 {
                eprintln!("Failed to get SIGBUS handler");
                std::process::exit(-6);
            }
            if sigaction.sa_flags & libc::SA_ONSTACK != libc::SA_ONSTACK {
                eprintln!("Expected SIGBUS handler to have SA_ONSTACK");
                std::process::exit(-7);
            }

            // Second, SIGSEGV
            let res = unsafe { libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut sigaction) };
            if res != 0 {
                eprintln!("Failed to get SIGSEGV handler");
                std::process::exit(-8);
            }
            if sigaction.sa_flags & libc::SA_ONSTACK != libc::SA_ONSTACK {
                eprintln!("Expected SIGSEGV handler to have SA_ONSTACK");
                std::process::exit(-9);
            }

            // OK, we're done
            std::process::exit(42);
        }
        pid => {
            // Parent process
            let mut status = 0;
            let _ = unsafe { libc::waitpid(pid, &mut status, 0) };

            // `status` is not the exit code, gotta unwrap some layers
            if libc::WIFEXITED(status) {
                let exit_code = libc::WEXITSTATUS(status);
                assert_eq!(exit_code, 42, "Child process exited with unexpected status");
            } else {
                panic!("Child process did not exit normally");
            }
        }
    }

    // OK, we're done
    Ok(())
}

#[cfg_attr(miri, ignore)]
#[cfg(target_os = "linux")]
#[test]
fn test_altstack_use_nocreate() -> anyhow::Result<()> {
    // Similar to the other test, this one operates inside of a fork in order to prevent poisoning
    // the main process state.
    use crate::StacktraceCollection;
    use chrono::Utc;
    use ddcommon_net1::Endpoint;

    let time = Utc::now().to_rfc3339();
    let dir = "/tmp/crashreports/";
    let output_url = format!("file://{dir}{time}.txt");

    let endpoint = Some(Endpoint::from_slice(&output_url));

    let path_to_receiver_binary =
        "/tmp/libdatadog/bin/libdatadog-crashtracking-receiver".to_string();
    let create_alt_stack = false; // Use, but do _not_ create
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
        None,
    )?;
    let metadata = CrashtrackerMetadata::new(
        "libname".to_string(),
        "version".to_string(),
        "family".to_string(),
        vec![],
    );

    // At this point we fork, because we're going to be looking at process-level state
    match unsafe { libc::fork() } {
        -1 => {
            panic!("Failed to fork");
        }
        0 => {
            // Child process
            // Get the current state of the altstack
            let initial_sigaltstack = get_sigaltstack();
            assert!(
                initial_sigaltstack.is_some(),
                "Failed to get initial sigaltstack"
            );

            // Initialize crashtracking.  This will
            // - create a new altstack
            // - set the SIGUBS/SIGSEGV handlers with SA_ONSTACK
            init(config, receiver_config, metadata)?;

            // Get the state of the altstack after initialization
            let after_init_sigaltstack = get_sigaltstack();

            // Compare the initial and after-init sigaltstacks:  they should be the same!
            if initial_sigaltstack != after_init_sigaltstack {
                eprintln!("Initial sigaltstack: {:?}", initial_sigaltstack);
                std::process::exit(-5);
            }

            // Even though the other test checks for the SA_ONSTACK flag on the signal handlers, we
            // double-check here because the options need to be decoupled
            let mut sigaction = libc::sigaction {
                sa_sigaction: 0,
                sa_mask: unsafe { std::mem::zeroed::<libc::sigset_t>() },
                sa_flags: 0,
                sa_restorer: None,
            };

            // First, SIGBUS
            let res = unsafe { libc::sigaction(libc::SIGBUS, std::ptr::null(), &mut sigaction) };
            if res != 0 {
                eprintln!("Failed to get SIGBUS handler");
                std::process::exit(-6);
            }
            if sigaction.sa_flags & libc::SA_ONSTACK != libc::SA_ONSTACK {
                eprintln!("Expected SIGBUS handler to have SA_ONSTACK");
                std::process::exit(-7);
            }

            // Second, SIGSEGV
            let res = unsafe { libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut sigaction) };
            if res != 0 {
                eprintln!("Failed to get SIGSEGV handler");
                std::process::exit(-8);
            }
            if sigaction.sa_flags & libc::SA_ONSTACK != libc::SA_ONSTACK {
                eprintln!("Expected SIGSEGV handler to have SA_ONSTACK");
                std::process::exit(-9);
            }

            // OK, we're done
            std::process::exit(42);
        }
        pid => {
            // Parent process
            let mut status = 0;
            let _ = unsafe { libc::waitpid(pid, &mut status, 0) };

            // `status` is not the exit code, gotta unwrap some layers
            if libc::WIFEXITED(status) {
                let exit_code = libc::WEXITSTATUS(status);
                assert_eq!(exit_code, 42, "Child process exited with unexpected status");
            } else {
                panic!("Child process did not exit normally");
            }
        }
    }

    // OK, we're done
    Ok(())
}

#[cfg_attr(miri, ignore)]
#[cfg(target_os = "linux")]
#[test]
fn test_altstack_nouse() -> anyhow::Result<()> {
    // This checks that when we do not request the altstack, we do not get the altstack
    use crate::StacktraceCollection;
    use chrono::Utc;
    use ddcommon_net1::Endpoint;

    let time = Utc::now().to_rfc3339();
    let dir = "/tmp/crashreports/";
    let output_url = format!("file://{dir}{time}.txt");

    let endpoint = Some(Endpoint::from_slice(&output_url));

    let path_to_receiver_binary =
        "/tmp/libdatadog/bin/libdatadog-crashtracking-receiver".to_string();
    let create_alt_stack = false;
    let use_alt_stack = false;
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
        None,
    )?;
    let metadata = CrashtrackerMetadata::new(
        "libname".to_string(),
        "version".to_string(),
        "family".to_string(),
        vec![],
    );

    // At this point we fork, because we're going to be looking at process-level state
    match unsafe { libc::fork() } {
        -1 => {
            panic!("Failed to fork");
        }
        0 => {
            // Child process
            // Get the current state of the altstack
            let initial_sigaltstack = get_sigaltstack();
            assert!(
                initial_sigaltstack.is_some(),
                "Failed to get initial sigaltstack"
            );

            // Initialize crashtracking.  This will
            // - create a new altstack
            // - set the SIGUBS/SIGSEGV handlers with SA_ONSTACK
            init(config, receiver_config, metadata)?;

            // Get the state of the altstack after initialization
            let after_init_sigaltstack = get_sigaltstack();

            // Compare the initial and after-init sigaltstacks:  they should be the same because we
            // did not enable anything!  This checks that we don't erroneously build the altstack.
            if initial_sigaltstack != after_init_sigaltstack {
                eprintln!("Initial sigaltstack: {:?}", initial_sigaltstack);
                std::process::exit(-5);
            }

            // Similarly, we need to be extra sure that SA_ONSTACK is not present.
            let mut sigaction = libc::sigaction {
                sa_sigaction: 0,
                sa_mask: unsafe { std::mem::zeroed::<libc::sigset_t>() },
                sa_flags: 0,
                sa_restorer: None,
            };

            // First, SIGBUS
            let res = unsafe { libc::sigaction(libc::SIGBUS, std::ptr::null(), &mut sigaction) };
            if res != 0 {
                eprintln!("Failed to get SIGBUS handler");
                std::process::exit(-6);
            }
            if sigaction.sa_flags & libc::SA_ONSTACK == libc::SA_ONSTACK {
                eprintln!("Expected SIGBUS handler not to have SA_ONSTACK");
                std::process::exit(-7);
            }

            // Second, SIGSEGV
            let res = unsafe { libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut sigaction) };
            if res != 0 {
                eprintln!("Failed to get SIGSEGV handler");
                std::process::exit(-8);
            }
            if sigaction.sa_flags & libc::SA_ONSTACK == libc::SA_ONSTACK {
                eprintln!("Expected SIGSEGV handler not to have SA_ONSTACK");
                std::process::exit(-9);
            }

            // OK, we're done
            std::process::exit(42);
        }
        pid => {
            // Parent process
            let mut status = 0;
            let _ = unsafe { libc::waitpid(pid, &mut status, 0) };

            // `status` is not the exit code, gotta unwrap some layers
            if libc::WIFEXITED(status) {
                let exit_code = libc::WEXITSTATUS(status);
                assert_eq!(exit_code, 42, "Child process exited with unexpected status");
            } else {
                panic!("Child process did not exit normally");
            }
        }
    }

    // OK, we're done
    Ok(())
}

#[cfg_attr(miri, ignore)]
#[cfg(target_os = "linux")]
#[test]
#[ignore]
fn test_waitall_nohang() -> anyhow::Result<()> {
    // This test checks whether the crashtracking implementation can cause malformed `waitall()`
    // idioms to hang.
    // Consider the following code from the Ruby runtime:
    //
    //   static VALUE
    //   proc_waitall(VALUE _)
    //   {
    //       VALUE result;
    //       rb_pid_t pid;
    //       int status;
    //
    //       result = rb_ary_new();
    //       rb_last_status_clear();
    //
    //       for (pid = -1;;) {
    //           pid = rb_waitpid(-1, &status, 0);
    //           if (pid == -1) {
    //               int e = errno;
    //               if (e == ECHILD)
    //                   break;
    //               rb_syserr_fail(e, 0);
    //           }
    //           rb_ary_push(result, rb_assoc_new(PIDT2NUM(pid), rb_last_status_get()));
    //       }
    //       return result;
    //   }
    //
    // The intent here is to wait for all of one's child processes to exit.  This is a pretty
    // standard operation in multi-process situations, with one important caveat:  usually you know
    // your children ahead of time and can wait on them in a controlled, intentional matter.
    // Previous versions of crashtracking, which spawned long-lived receiver processes, would
    // interfere with this
    //
    // This implements the inner behavior of a test which allows the caller to control which
    // options are used.
    use crate::StacktraceCollection;
    use chrono::Utc;
    use ddcommon_net1::Endpoint;

    let time = Utc::now().to_rfc3339();
    let dir = "/tmp/crashreports/";
    let output_url = format!("file://{dir}{time}.txt");

    let endpoint = Some(Endpoint::from_slice(&output_url));

    let path_to_receiver_binary =
        "/tmp/libdatadog/bin/libdatadog-crashtracking-receiver".to_string();
    let create_alt_stack = true; // doesn't matter, but most runtimes use it, so take it
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
        None,
    )?;

    let metadata = CrashtrackerMetadata::new(
        "libname".to_string(),
        "version".to_string(),
        "family".to_string(),
        vec![],
    );

    // Since this test ultimately mutates process state, it's done inside of a fork just like the
    // other tests of the same ilk.
    match unsafe { libc::fork() } {
        -1 => {
            panic!("Failed to fork");
        }
        0 => {
            // Child process
            // This is where the test actually happens!
            init(config, receiver_config, metadata)?;

            // Now spawn some short-lived child processes.
            // Note:  it's easy to confirm this test actually works by cranking the sleep duration
            // up past the timeout duration. At such a point, the test should fail.
            let mut children = vec![];
            let sleep_duration_ms = 100;
            let timeout_duration_ms = 150;
            for _ in 0..10 {
                match unsafe { libc::fork() } {
                    -1 => {
                        panic!("Failed to fork");
                    }
                    0 => {
                        // Grandchild process
                        std::thread::sleep(std::time::Duration::from_millis(sleep_duration_ms));
                        std::process::exit(0); // normal exit, since we're testing waitall
                    }
                    pid => {
                        // Parent process
                        children.push(pid); // unused in this test
                    }
                }
            }

            // Now, do the equivalent of the waitall loop.
            // One caveat is that we do not want to hang the test, so rather than an unbounded
            // `waitpid()`, use WNOHANG within a timer loop.
            let timeout = std::time::Duration::from_millis(timeout_duration_ms);
            let start_time = std::time::Instant::now();
            loop {
                if start_time.elapsed() > timeout {
                    eprintln!("Timed out waiting for children to exit");
                    std::process::exit(-6);
                }

                // Call waitpid with WNOHANG
                let mut status = 0;
                let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
                let errno = std::io::Error::last_os_error().raw_os_error().unwrap();

                if pid == -1 && errno == libc::ECHILD {
                    // No more children!  Done!
                    std::process::exit(42);
                }
            }
        }
        pid => {
            // Parent process
            let mut status = 0;
            let _ = unsafe { libc::waitpid(pid, &mut status, 0) };

            // `status` is not the exit code, gotta unwrap some layers
            if libc::WIFEXITED(status) {
                let exit_code = libc::WEXITSTATUS(status);
                assert_eq!(exit_code, 42, "Child process exited with unexpected status");
            } else {
                panic!("Child process did not exit normally");
            }
        }
    }

    // Confirmed that we have no children, so we're done.
    Ok(())
}
