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
    use std::{env, time::Duration};

    use datadog_crashtracker::{
        self as crashtracker, CrashtrackerConfiguration, CrashtrackerMetadata,
        CrashtrackerReceiverConfig,
    };
    use ddcommon::tag;

    #[inline(never)]
    unsafe fn deref_ptr(p: *mut u8) {
        *std::hint::black_box(p) = std::hint::black_box(1);
    }

    pub fn main() -> anyhow::Result<()> {
        let mut args = env::args().skip(1);
        let output_url = args.next().context("Unexpected number of arguments")?;
        let receiver_binary = args.next().context("Unexpected number of arguments")?;
        let stderr_filename = args.next().context("Unexpected number of arguments")?;
        let stdout_filename = args.next().context("Unexpected number of arguments")?;
        let timeout = Duration::from_secs(30);

        let endpoint = if output_url.is_empty() {
            None
        } else {
            Some(ddcommon::Endpoint {
                url: ddcommon::parse_uri(&output_url)?,
                api_key: None,
            })
        };

        crashtracker::init(
            CrashtrackerConfiguration {
                additional_files: vec![],
                create_alt_stack: true,
                resolve_frames: crashtracker::StacktraceCollection::WithoutSymbols,
                endpoint,
                timeout,
            },
            Some(CrashtrackerReceiverConfig::new(
                vec![],
                env::vars().collect(),
                receiver_binary,
                Some(stderr_filename),
                Some(stdout_filename),
            )?),
            CrashtrackerMetadata {
                profiling_library_name: "libdatadog".to_owned(),
                profiling_library_version: "1.0.0".to_owned(),
                family: "native".to_owned(),
                tags: vec![
                    tag!("service", "foo"),
                    tag!("service_version", "bar"),
                    tag!("runtime-id", "xyz"),
                    tag!("language", "native"),
                ],
            },
        )?;
        crashtracker::begin_profiling_op(crashtracker::ProfilingOpTypes::CollectingSample)?;
        unsafe {
            deref_ptr(std::ptr::null_mut::<u8>());
        }
        crashtracker::end_profiling_op(crashtracker::ProfilingOpTypes::CollectingSample)?;
        Ok(())
    }
}
