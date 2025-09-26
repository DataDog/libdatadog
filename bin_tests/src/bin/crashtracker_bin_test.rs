// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(not(unix))]
fn main() {}

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    unix::main()
}

#[cfg(unix)]
mod unix {
    use anyhow::Context;
    use bin_tests::modes::behavior::get_behavior;
    use nix::{
        sys::signal::{kill, raise, Signal},
        unistd::Pid,
    };
    use std::env;
    use std::path::Path;
    use std::time::Duration;

    use datadog_crashtracker::{
        self as crashtracker, register_runtime_stack_callback, CrashtrackerConfiguration,
        CrashtrackerReceiverConfig, Metadata, RuntimeStackFrame,
    };
    use ddcommon::{tag, Endpoint};
    use std::ffi::{c_char, c_void, CString};

    const TEST_COLLECTOR_TIMEOUT: Duration = Duration::from_secs(10);

    #[inline(never)]
    pub unsafe fn cause_segfault() -> anyhow::Result<()> {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            std::arch::asm!("mov eax, [0]", options(nostack));
        }

        #[cfg(target_arch = "aarch64")]
        {
            std::arch::asm!("mov x0, #0", "ldr x1, [x0]", options(nostack));
        }
        anyhow::bail!("Failed to cause segmentation fault")
    }

    // Simulated Python/Ruby runtime callback for testing
    unsafe extern "C" fn test_runtime_callback(
        emit_frame: unsafe extern "C" fn(*const RuntimeStackFrame),
        _context: *mut c_void,
    ) {
        // Use static strings for signal safety - in a real runtime these would be
        // pointers to strings in the runtime's managed memory
        let frames = [
            // Application frame
            RuntimeStackFrame {
                function_name: b"handle_request\0".as_ptr() as *const c_char,
                file_name: b"app.py\0".as_ptr() as *const c_char,
                line_number: 45,
                column_number: 12,
                class_name: b"RequestHandler\0".as_ptr() as *const c_char,
                module_name: b"myapp\0".as_ptr() as *const c_char,
            },
            // Framework frame
            RuntimeStackFrame {
                function_name: b"process_request\0".as_ptr() as *const c_char,
                file_name: b"framework/web.py\0".as_ptr() as *const c_char,
                line_number: 123,
                column_number: 8,
                class_name: b"WebFramework\0".as_ptr() as *const c_char,
                module_name: b"framework\0".as_ptr() as *const c_char,
            },
            // Library frame
            RuntimeStackFrame {
                function_name: b"db_query\0".as_ptr() as *const c_char,
                file_name: b"lib/database.py\0".as_ptr() as *const c_char,
                line_number: 67,
                column_number: 15,
                class_name: b"DatabaseConnection\0".as_ptr() as *const c_char,
                module_name: b"database\0".as_ptr() as *const c_char,
            },
            // Core runtime frame (no class/module for C code)
            RuntimeStackFrame {
                function_name: b"_execute_bytecode\0".as_ptr() as *const c_char,
                file_name: b"python/eval.c\0".as_ptr() as *const c_char,
                line_number: 2341,
                column_number: 0,
                class_name: std::ptr::null(),
                module_name: std::ptr::null(),
            },
        ];

        // Emit each frame
        for frame in &frames {
            emit_frame(frame);
        }
    }

    pub fn main() -> anyhow::Result<()> {
        let mut args = env::args().skip(1);
        let output_url = args.next().context("Unexpected number of arguments")?;
        let receiver_binary = args.next().context("Unexpected number of arguments")?;
        let output_dir = args.next().context("Unexpected number of arguments")?;
        let mode_str = args.next().context("Unexpected number of arguments")?;
        let crash_typ = args.next().context("Missing crash type")?;
        anyhow::ensure!(args.next().is_none(), "unexpected extra arguments");

        let stderr_filename = format!("{output_dir}/out.stderr");
        let stdout_filename = format!("{output_dir}/out.stdout");
        let output_dir: &Path = output_dir.as_ref();

        let endpoint = if output_url.is_empty() {
            None
        } else {
            Some(Endpoint::from_slice(&output_url))
        };

        // The configuration can be modified by a Behavior (testing plan), so it is mut here.
        // Unlike a normal harness, in this harness tests are run in individual processes, so race
        // issues are avoided.
        let mut config = CrashtrackerConfiguration::new(
            vec![],
            true,
            true,
            endpoint,
            crashtracker::StacktraceCollection::WithoutSymbols,
            crashtracker::default_signals(),
            Some(TEST_COLLECTOR_TIMEOUT),
            Some("".to_string()),
            true,
        )?;

        let metadata = Metadata {
            library_name: "libdatadog".to_owned(),
            library_version: "1.0.0".to_owned(),
            family: "native".to_owned(),
            tags: vec![
                tag!("service", "foo"),
                tag!("service_version", "bar"),
                tag!("runtime-id", "xyz"),
                tag!("language", "native"),
            ]
            .into_iter()
            .map(|x| x.to_string())
            .collect(),
        };

        // Set the behavior of the test, run setup, and do the pre-init test
        let behavior = get_behavior(&mode_str);
        behavior.setup(output_dir, &mut config)?;
        behavior.pre(output_dir)?;

        crashtracker::init(
            config,
            CrashtrackerReceiverConfig::new(
                vec![],
                env::vars().collect(),
                receiver_binary,
                Some(stderr_filename),
                Some(stdout_filename),
            )?,
            metadata,
        )?;

        // Conduct the post-init test
        behavior.post(output_dir)?;

        crashtracker::begin_op(crashtracker::OpTypes::ProfilerCollectingSample)?;
        match crash_typ.as_str() {
            "kill_sigabrt" => kill(Pid::this(), Signal::SIGABRT)?,
            "kill_sigill" => kill(Pid::this(), Signal::SIGILL)?,
            "kill_sigbus" => kill(Pid::this(), Signal::SIGBUS)?,
            "kill_sigsegv" => kill(Pid::this(), Signal::SIGSEGV)?,
            "null_deref" => unsafe { cause_segfault()? },
            "raise_sigabrt" => raise(Signal::SIGABRT)?,
            "raise_sigill" => raise(Signal::SIGILL)?,
            "raise_sigbus" => raise(Signal::SIGBUS)?,
            "raise_sigsegv" => raise(Signal::SIGSEGV)?,
            "runtime_callback_test" => {
                // Register runtime callback to simulate Python/Ruby runtime integration
                register_runtime_stack_callback(test_runtime_callback, std::ptr::null_mut())
                    .map_err(|e| anyhow::anyhow!("Failed to register runtime callback: {:?}", e))?;
                eprintln!("Runtime callback registered successfully");

                // Cause a segfault to trigger crash handling with runtime callback
                unsafe { cause_segfault()? }
            }
            _ => anyhow::bail!("Unexpected crash_typ: {crash_typ}"),
        }
        crashtracker::end_op(crashtracker::OpTypes::ProfilerCollectingSample)?;
        Ok(())
    }
}
