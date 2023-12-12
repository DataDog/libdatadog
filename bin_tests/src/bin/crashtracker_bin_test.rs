// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use std::env;

use datadog_profiling::{
    crashtracker::{self, CrashtrackerConfiguration, CrashtrackerMetadata},
    exporter::Tag,
};

fn main() -> anyhow::Result<()> {
    let mut args = env::args().skip(1);
    let output_filename = args
        .next()
        .ok_or(anyhow::anyhow!("Unexpected number of arguments"))?;
    let receiver_binary = args
        .next()
        .ok_or(anyhow::anyhow!("Unexpected number of arguments"))?;
    crashtracker::init(
        CrashtrackerConfiguration {
            create_alt_stack: true,
            endpoint: Some(datadog_profiling::exporter::Endpoint {
                url: hyper::Uri::from_maybe_shared(format!("file://{}", output_filename))?,
                api_key: None,
            }),
            path_to_receiver_binary: receiver_binary,
            resolve_frames: crashtracker::CrashtrackerResolveFrames::ExperimentalInProcess,
            stderr_filename: None,
            stdout_filename: None,
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
        *std::hint::black_box(std::ptr::null_mut::<u8>()) = 1;
    }
    crashtracker::end_profiling_op(crashtracker::ProfilingOpTypes::CollectingSample)?;
    Ok(())
}
