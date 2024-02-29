// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

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
    };
    use datadog_profiling::exporter::Tag;

    #[inline(never)]
    unsafe fn deref_ptr(p: *mut u8) {
        *std::hint::black_box(p) = std::hint::black_box(1);
    }

    pub fn main() -> anyhow::Result<()> {
        let mut args = env::args().skip(1);
        let output_filename = args.next().context("Unexpected number of arguments")?;
        let receiver_binary = args.next().context("Unexpected number of arguments")?;
        let stderr_filename = args.next().context("Unexpected number of arguments")?;
        let stdout_filename = args.next().context("Unexpected number of arguments")?;
        crashtracker::init(
            CrashtrackerConfiguration {
                create_alt_stack: true,
                endpoint: Some(datadog_profiling::exporter::Endpoint {
                    url: ddcommon::parse_uri(&format!("file://{}", output_filename))?,
                    api_key: None,
                }),
                path_to_receiver_binary: receiver_binary,
                resolve_frames: crashtracker::CrashtrackerResolveFrames::Never,
                stderr_filename: Some(stderr_filename),
                stdout_filename: Some(stdout_filename),
                collect_stacktrace: true,
            },
            CrashtrackerMetadata {
                profiling_library_name: "libdatadog".to_owned(),
                profiling_library_version: "1.0.0".to_owned(),
                family: "native".to_owned(),
                tags: vec![
                    Tag::new("service", "foo").unwrap(),
                    Tag::new("service_version", "bar").unwrap(),
                    Tag::new("runtime-id", "xyz").unwrap(),
                    Tag::new("language", "native").unwrap(),
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
