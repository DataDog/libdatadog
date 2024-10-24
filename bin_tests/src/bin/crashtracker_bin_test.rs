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
    use std::env;

    use datadog_crashtracker::{
        self as crashtracker, CrashtrackerConfiguration, CrashtrackerMetadata,
        CrashtrackerReceiverConfig,
    };
    use ddcommon::{tag, Endpoint};

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
        anyhow::ensure!(args.next().is_none(), "unexpected extra arguments");

        let endpoint = if output_url.is_empty() {
            None
        } else {
            Some(Endpoint::from_slice(&output_url))
        };

        let config = CrashtrackerConfiguration {
            additional_files: vec![],
            create_alt_stack: true,
            resolve_frames: crashtracker::StacktraceCollection::WithoutSymbols,
            endpoint,
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

        crashtracker::begin_op(crashtracker::OpTypes::ProfilerCollectingSample)?;
        unsafe {
            deref_ptr(std::ptr::null_mut::<u8>());
        }
        crashtracker::end_op(crashtracker::OpTypes::ProfilerCollectingSample)?;
        Ok(())
    }
}
