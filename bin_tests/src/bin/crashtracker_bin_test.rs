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
    use std::process;
    use std::time::Duration;

    use libdd_common::{tag, Endpoint};
    use libdd_crashtracker::{
        self as crashtracker, CrashtrackerConfiguration, CrashtrackerReceiverConfig, Metadata,
    };

    const TEST_COLLECTOR_TIMEOUT: Duration = Duration::from_secs(15);

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

    pub fn main() -> anyhow::Result<()> {
        let raw_args: Vec<String> = env::args().collect();
        let mut args = raw_args.iter().skip(1);
        let output_url = args.next().context("Unexpected number of arguments")?;
        let receiver_binary = args.next().context("Unexpected number of arguments")?;
        let output_dir = args.next().context("Unexpected number of arguments")?;
        let mode_str = args.next().context("Unexpected number of arguments")?;
        let crash_typ = args.next().context("Missing crash type")?;
        anyhow::ensure!(args.next().is_none(), "unexpected extra arguments");

        // For preload logger mode, ensure we actually start with LD_PRELOAD applied.
        // Setting LD_PRELOAD after startup has no effect on the current process,
        // so re-exec only if we weren't born with it
        if mode_str == "runtime_preload_logger" && env::var_os("LD_PRELOAD").is_none() {
            if let Some(so_path) = option_env!("PRELOAD_LOGGER_SO") {
                let status = process::Command::new(&raw_args[0])
                    .args(&raw_args[1..])
                    .env("LD_PRELOAD", so_path)
                    .status()
                    .context("failed to re-exec with LD_PRELOAD")?;
                let code = status.code().unwrap_or(1);
                process::exit(code);
            }
        }

        let stderr_filename = format!("{output_dir}/out.stderr");
        let stdout_filename = format!("{output_dir}/out.stdout");
        let output_dir: &Path = output_dir.as_ref();

        let endpoint = if output_url.is_empty() {
            None
        } else {
            Some(Endpoint::from_slice(output_url))
        };

        // The configuration can be modified by a Behavior (testing plan), so it is mut here.
        // Unlike a normal harness, in this harness tests are run in individual processes, so race
        // issues are avoided.
        let stacktrace_collection = match env::var("DD_TEST_STACKTRACE_COLLECTION") {
            Ok(val) => match val.as_str() {
                "disabled" => crashtracker::StacktraceCollection::Disabled,
                "without_symbols" => crashtracker::StacktraceCollection::WithoutSymbols,
                "inprocess_symbols" => {
                    crashtracker::StacktraceCollection::EnabledWithInprocessSymbols
                }
                "receiver_symbols" => {
                    crashtracker::StacktraceCollection::EnabledWithSymbolsInReceiver
                }
                _ => crashtracker::StacktraceCollection::WithoutSymbols,
            },
            Err(_) => crashtracker::StacktraceCollection::WithoutSymbols,
        };

        let mut config = CrashtrackerConfiguration::new(
            vec![],
            true,
            true,
            endpoint,
            stacktrace_collection,
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
        let behavior = get_behavior(mode_str);
        behavior.setup(output_dir, &mut config)?;
        behavior.pre(output_dir)?;

        crashtracker::init(
            config,
            CrashtrackerReceiverConfig::new(
                vec![],
                env::vars().collect(),
                receiver_binary.to_string(),
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
            _ => anyhow::bail!("Unexpected crash_typ: {crash_typ}"),
        }
        crashtracker::end_op(crashtracker::OpTypes::ProfilerCollectingSample)?;
        Ok(())
    }
}
