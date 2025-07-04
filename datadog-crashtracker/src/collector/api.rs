// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg(unix)]

use super::{crash_handler::enable, receiver_manager::Receiver};
use crate::{
    clear_spans, clear_traces, collector::signal_handler_manager::register_crash_handlers,
    crash_info::Metadata, reset_counters, shared::configuration::CrashtrackerReceiverConfig,
    update_config, update_metadata, CrashtrackerConfiguration,
};

pub static DEFAULT_SYMBOLS: [libc::c_int; 4] =
    [libc::SIGBUS, libc::SIGABRT, libc::SIGSEGV, libc::SIGILL];

pub fn default_signals() -> Vec<libc::c_int> {
    Vec::from(DEFAULT_SYMBOLS)
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
    metadata: Metadata,
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
    Receiver::update_stored_config(receiver_config)?;
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
    metadata: Metadata,
) -> anyhow::Result<()> {
    update_metadata(metadata)?;
    update_config(config.clone())?;
    Receiver::update_stored_config(receiver_config)?;
    register_crash_handlers(&config)?;
    enable();
    Ok(())
}

/// Reconfigure the crash-tracking infrastructure.
///
/// PRECONDITIONS:
///     None.
/// SAFETY:
///     Crash-tracking functions are not reentrant.
///     No other crash-handler functions should be called concurrently.
/// ATOMICITY:
///     This function is not atomic. A crash during its execution may lead to
///     unexpected crash-handling behaviour.
pub fn reconfigure(
    config: CrashtrackerConfiguration,
    receiver_config: CrashtrackerReceiverConfig,
    metadata: Metadata,
) -> anyhow::Result<()> {
    update_metadata(metadata)?;
    update_config(config.clone())?;
    Receiver::update_stored_config(receiver_config)?;
    enable();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{begin_op, insert_span, insert_trace, StacktraceCollection};
    use chrono::Utc;
    use ddcommon::tag;
    use ddcommon::Endpoint;
    use std::time::Duration;
    // We can't run this in the main test runner because it (deliberately) crashes,
    // and would make all following tests unrunable.
    // To run this test,
    // ./build-profiling-ffi /tmp/libdatadog
    // mkdir /tmp/crashreports
    // look in /tmp/crashreports for the crash reports and output files
    #[ignore]
    #[test]
    fn test_crash() {
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
        let timeout = Duration::from_secs(10);
        let receiver_config = CrashtrackerReceiverConfig::new(
            vec![],
            vec![],
            path_to_receiver_binary,
            stderr_filename,
            stdout_filename,
        )
        .unwrap();
        let config = CrashtrackerConfiguration::new(
            vec![],
            create_alt_stack,
            use_alt_stack,
            endpoint,
            resolve_frames,
            default_signals(),
            Some(timeout),
            None,
            true,
        )
        .unwrap();
        let metadata = Metadata::new(
            "libname".to_string(),
            "version".to_string(),
            "family".to_string(),
            vec![],
        );
        init(config, receiver_config, metadata).unwrap();
        begin_op(crate::OpTypes::ProfilerCollectingSample).unwrap();
        insert_span(42).unwrap();
        insert_trace(u128::MAX).unwrap();
        insert_span(12).unwrap();
        insert_trace(99399939399939393993).unwrap();

        let tag = tag!("apple", "banana");
        let metadata2 = Metadata::new(
            "libname".to_string(),
            "version".to_string(),
            "family".to_string(),
            vec![tag.to_string()],
        );
        update_metadata(metadata2).expect("metadata");

        std::thread::sleep(Duration::from_secs(2));

        let p: *const u32 = std::ptr::null();
        let q = unsafe { *p };
        assert_eq!(q, 3);
    }

    #[test]
    fn test_altstack_paradox() {
        let time = Utc::now().to_rfc3339();
        let dir = "/tmp/crashreports/";
        let output_url = format!("file://{dir}{time}.txt");

        let endpoint = Some(Endpoint::from_slice(&output_url));

        let create_alt_stack = true;
        let use_alt_stack = false;
        let resolve_frames = StacktraceCollection::EnabledWithInprocessSymbols;
        let timeout = Duration::from_secs(10);

        // This should return an error, because we're creating an altstack without using it
        let config = CrashtrackerConfiguration::new(
            vec![],
            create_alt_stack,
            use_alt_stack,
            endpoint,
            resolve_frames,
            default_signals(),
            Some(timeout),
            None,
            true,
        );

        // This is slightly over-tuned to the language of the error message, but it'd require some
        // novel engineering just for this test in order to tighten this up.
        let err = config.unwrap_err();
        assert_eq!(
            err.to_string(),
            "Cannot create an altstack without using it"
        );
    }

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
    fn test_altstack_use_create() {
        // This test initializes crashtracking in a fork, then waits on the exit status of the
        // child. We check for an atypical exit status in order to ensure that only our
        // desired exit path is taken.

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
        let signals = default_signals();
        let timeout = Duration::from_secs(10);
        let receiver_config = CrashtrackerReceiverConfig::new(
            vec![],
            vec![],
            path_to_receiver_binary,
            stderr_filename,
            stdout_filename,
        )
        .unwrap();
        let config = CrashtrackerConfiguration::new(
            vec![],
            create_alt_stack,
            use_alt_stack,
            endpoint,
            resolve_frames,
            signals,
            Some(timeout),
            None,
            true,
        )
        .unwrap();
        let metadata = Metadata::new(
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
                init(config, receiver_config, metadata).unwrap();

                // Get the state of the altstack after initialization
                let after_init_sigaltstack = get_sigaltstack();

                // Compare the initial and after-init sigaltstacks
                if initial_sigaltstack == after_init_sigaltstack {
                    eprintln!("Initial sigaltstack: {initial_sigaltstack:?}");
                    std::process::exit(-5);
                }

                // Check the SIGBUS and SIGSEGV handlers are set with SA_ONSTACK
                let mut sigaction = libc::sigaction {
                    sa_sigaction: 0,
                    sa_mask: unsafe { std::mem::zeroed::<libc::sigset_t>() },
                    sa_flags: 0,
                    sa_restorer: None,
                };

                let mut exit_code = -5;

                for signal in default_signals() {
                    let signame = crate::signal_from_signum(signal).unwrap();
                    exit_code -= 1;
                    let res = unsafe { libc::sigaction(signal, std::ptr::null(), &mut sigaction) };
                    if res != 0 {
                        eprintln!("Failed to get {signame:?} handler");
                        std::process::exit(exit_code);
                    }

                    exit_code -= 1;
                    if sigaction.sa_flags & libc::SA_ONSTACK != libc::SA_ONSTACK {
                        eprintln!("Expected {signame:?} handler to have SA_ONSTACK");
                        std::process::exit(exit_code);
                    }
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
    }

    #[cfg_attr(miri, ignore)]
    #[cfg(target_os = "linux")]
    #[test]
    fn test_altstack_use_nocreate() {
        // Similar to the other test, this one operates inside of a fork in order to prevent
        // poisoning the main process state.

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
        let signals = default_signals();
        let timeout = Duration::from_secs(10);
        let receiver_config = CrashtrackerReceiverConfig::new(
            vec![],
            vec![],
            path_to_receiver_binary,
            stderr_filename,
            stdout_filename,
        )
        .unwrap();
        let config = CrashtrackerConfiguration::new(
            vec![],
            create_alt_stack,
            use_alt_stack,
            endpoint,
            resolve_frames,
            signals,
            Some(timeout),
            None,
            true,
        )
        .unwrap();
        let metadata = Metadata::new(
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
                init(config, receiver_config, metadata).unwrap();

                // Get the state of the altstack after initialization
                let after_init_sigaltstack = get_sigaltstack();

                // Compare the initial and after-init sigaltstacks:  they should be the same!
                if initial_sigaltstack != after_init_sigaltstack {
                    eprintln!("Initial sigaltstack: {initial_sigaltstack:?}");
                    std::process::exit(-5);
                }

                // Even though the other test checks for the SA_ONSTACK flag on the signal handlers,
                // we double-check here because the options need to be decoupled
                let mut sigaction = libc::sigaction {
                    sa_sigaction: 0,
                    sa_mask: unsafe { std::mem::zeroed::<libc::sigset_t>() },
                    sa_flags: 0,
                    sa_restorer: None,
                };

                // First, SIGBUS
                let res =
                    unsafe { libc::sigaction(libc::SIGBUS, std::ptr::null(), &mut sigaction) };
                if res != 0 {
                    eprintln!("Failed to get SIGBUS handler");
                    std::process::exit(-6);
                }
                if sigaction.sa_flags & libc::SA_ONSTACK != libc::SA_ONSTACK {
                    eprintln!("Expected SIGBUS handler to have SA_ONSTACK");
                    std::process::exit(-7);
                }

                // Second, SIGSEGV
                let res =
                    unsafe { libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut sigaction) };
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
    }

    #[cfg_attr(miri, ignore)]
    #[cfg(target_os = "linux")]
    #[test]
    fn test_altstack_nouse() {
        // This checks that when we do not request the altstack, we do not get the altstack

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
        let signals = default_signals();
        let timeout = Duration::from_secs(10);
        let receiver_config = CrashtrackerReceiverConfig::new(
            vec![],
            vec![],
            path_to_receiver_binary,
            stderr_filename,
            stdout_filename,
        )
        .unwrap();
        let config = CrashtrackerConfiguration::new(
            vec![],
            create_alt_stack,
            use_alt_stack,
            endpoint,
            resolve_frames,
            signals,
            Some(timeout),
            None,
            true,
        )
        .unwrap();
        let metadata = Metadata::new(
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
                init(config, receiver_config, metadata).unwrap();

                // Get the state of the altstack after initialization
                let after_init_sigaltstack = get_sigaltstack();

                // Compare the initial and after-init sigaltstacks:  they should be the same because
                // we did not enable anything!  This checks that we don't
                // erroneously build the altstack.
                if initial_sigaltstack != after_init_sigaltstack {
                    eprintln!("Initial sigaltstack: {initial_sigaltstack:?}");
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
                let res =
                    unsafe { libc::sigaction(libc::SIGBUS, std::ptr::null(), &mut sigaction) };
                if res != 0 {
                    eprintln!("Failed to get SIGBUS handler");
                    std::process::exit(-6);
                }
                if sigaction.sa_flags & libc::SA_ONSTACK == libc::SA_ONSTACK {
                    eprintln!("Expected SIGBUS handler not to have SA_ONSTACK");
                    std::process::exit(-7);
                }

                // Second, SIGSEGV
                let res =
                    unsafe { libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut sigaction) };
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
    }

    #[cfg_attr(miri, ignore)]
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn test_waitall_nohang() {
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
        // standard operation in multi-process situations, with one important caveat:  usually you
        // know your children ahead of time and can wait on them in a controlled,
        // intentional matter. Previous versions of crashtracking, which spawned long-lived
        // receiver processes, would interfere with this
        //
        // This implements the inner behavior of a test which allows the caller to control which
        // options are used.

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
        let signals = default_signals();
        let timeout = Duration::from_secs(10);
        let receiver_config = CrashtrackerReceiverConfig::new(
            vec![],
            vec![],
            path_to_receiver_binary,
            stderr_filename,
            stdout_filename,
        )
        .unwrap();
        let config = CrashtrackerConfiguration::new(
            vec![],
            create_alt_stack,
            use_alt_stack,
            endpoint,
            resolve_frames,
            signals,
            Some(timeout),
            None,
            true,
        )
        .unwrap();

        let metadata = Metadata::new(
            "libname".to_string(),
            "version".to_string(),
            "family".to_string(),
            vec![],
        );

        // Since this test ultimately mutates process state, it's done inside of a fork just like
        // the other tests of the same ilk.
        match unsafe { libc::fork() } {
            -1 => {
                panic!("Failed to fork");
            }
            0 => {
                // Child process
                // This is where the test actually happens!
                init(config, receiver_config, metadata).unwrap();

                // Now spawn some short-lived child processes.
                // Note:  it's easy to confirm this test actually works by cranking the sleep
                // duration up past the timeout duration. At such a point, the test
                // should fail.
                let mut children = vec![];
                let sleep_duration = Duration::from_millis(100);
                let timeout_duration = Duration::from_millis(150);
                for _ in 0..10 {
                    match unsafe { libc::fork() } {
                        -1 => {
                            panic!("Failed to fork");
                        }
                        0 => {
                            // Grandchild process
                            std::thread::sleep(sleep_duration);
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
                let start_time = std::time::Instant::now();
                loop {
                    if start_time.elapsed() > timeout_duration {
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
    }
}
