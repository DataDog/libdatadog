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
        self as crashtracker, CrashtrackerConfiguration, CrashtrackerReceiverConfig, Metadata,
    };
    use libdd_common::{tag, Endpoint};

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
            _ => anyhow::bail!("Unexpected crash_typ: {crash_typ}"),
        }
        crashtracker::end_op(crashtracker::OpTypes::ProfilerCollectingSample)?;
        Ok(())
    }
}
