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
    use std::env;
    use std::path::Path;

    use datadog_crashtracker::{
        self as crashtracker, CrashtrackerConfiguration, CrashtrackerMetadata,
        CrashtrackerReceiverConfig,
    };
    use ddcommon::tag;
    use ddcommon_net1::Endpoint;

    const TEST_COLLECTOR_TIMEOUT_MS: u32 = 10_000;

    #[inline(never)]
    unsafe fn deref_ptr(p: *mut u8) {
        *std::hint::black_box(p) = std::hint::black_box(1);
    }

    pub fn main() -> anyhow::Result<()> {
        let mut args = env::args().skip(1);
        let output_url = args.next().context("Unexpected number of arguments")?;
        let receiver_binary = args.next().context("Unexpected number of arguments")?;
        let output_dir = args.next().context("Unexpected number of arguments")?;
        let mode_str = args.next().context("Unexpected number of arguments")?;
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
        let mut config = CrashtrackerConfiguration {
            additional_files: vec![],
            create_alt_stack: true,
            use_alt_stack: true,
            resolve_frames: crashtracker::StacktraceCollection::WithoutSymbols,
            endpoint,
            timeout_ms: TEST_COLLECTOR_TIMEOUT_MS,
            unix_socket_path: Some("".to_string()),
        };

        let metadata = CrashtrackerMetadata {
            library_name: "libdatadog".to_owned(),
            library_version: "1.0.0".to_owned(),
            family: "native".to_owned(),
            tags: vec![
                tag!("service", "foo"),
                tag!("service_version", "bar"),
                tag!("runtime-id", "xyz"),
                tag!("language", "native"),
            ],
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
        unsafe {
            deref_ptr(std::ptr::null_mut::<u8>());
        }
        crashtracker::end_op(crashtracker::OpTypes::ProfilerCollectingSample)?;

        Ok(())
    }
}
