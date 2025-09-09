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
    use anyhow::ensure;
    use anyhow::Context;
    use std::env;
    use std::time::Duration;

    use datadog_crashtracker::{
        self as crashtracker, CrashtrackerConfiguration, CrashtrackerReceiverConfig, Metadata,
    };
    use ddcommon::{tag, Endpoint};

    const TEST_COLLECTOR_TIMEOUT: Duration = Duration::from_secs(10);

    #[inline(never)]
    unsafe fn fn3() {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            std::arch::asm!("mov eax, [0]", options(nostack));
        }

        #[cfg(target_arch = "aarch64")]
        {
            std::arch::asm!("mov x0, #0", "ldr x1, [x0]", options(nostack));
        }
    }

    #[inline(never)]
    fn fn2() {
        unsafe { fn3() }
    }

    #[inline(never)]
    fn fn1() {
        fn2()
    }

    #[inline(never)]
    pub fn main() -> anyhow::Result<()> {
        // init crashtracker
        let mut args = env::args().skip(1);
        let output_url = args.next().context("Unexpected number of arguments 1")?;
        let receiver_binary = args.next().context("Unexpected number of arguments 2")?;
        let output_dir = args.next().context("Unexpected number of arguments 3")?;
        anyhow::ensure!(args.next().is_none(), "unexpected extra arguments");

        let stderr_filename = format!("{output_dir}/out.stderr");
        let stdout_filename = format!("{output_dir}/out.stdout");

        ensure!(!output_url.is_empty(), "output_url must not be empty");
        let endpoint = Some(Endpoint::from_slice(&output_url));

        let config = CrashtrackerConfiguration::new(
            vec![], // additional_files
            true,   // create_alt_stack
            true,   // use_alt_stack
            endpoint,
            crashtracker::StacktraceCollection::EnabledWithSymbolsInReceiver,
            crashtracker::default_signals(),
            Some(TEST_COLLECTOR_TIMEOUT),
            Some("".to_string()), // unix_socket_path
            true,                 // demangle_names
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

        crashtracker::init(
            config,
            CrashtrackerReceiverConfig::new(
                vec![],                // args
                env::vars().collect(), // env
                receiver_binary,
                Some(stderr_filename),
                Some(stdout_filename),
            )?,
            metadata,
        )?;

        fn1();
        Ok(())
    }
}
