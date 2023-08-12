// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

mod profile_index;
mod replayer;

use clap::{command, Arg};
use datadog_profiling::profile;
use prost::Message;
use std::borrow::Cow;
use std::io::Cursor;
use std::time::Instant;

pub use replayer::*;

fn main() -> anyhow::Result<()> {
    let matches = command!()
        .arg(
            Arg::new("input")
                .short('i')
                .help("the pprof to replay")
                .required(true),
        )
        .arg(
            Arg::new("output")
                .short('o')
                .help("the path to save the result to")
                .required(false),
        )
        .get_matches();

    let input = matches.get_one::<String>("input").unwrap();
    let output = matches.get_one::<String>("output");

    let source = {
        println!("Reading in pprof from file '{input}'");
        std::fs::read(input)?
    };

    let pprof = profile::pprof::Profile::decode(&mut Cursor::new(source))?;

    let mut replayer = Replayer::try_from(&pprof)?;

    let mut outprof = profile::Profile::builder()
        .start_time(Some(replayer.start_time))
        .sample_types(replayer.sample_types.clone())
        .period(replayer.period.clone())
        .build();

    let samples = std::mem::take(&mut replayer.samples);
    let before = Instant::now();
    for sample in samples {
        outprof.add(sample)?;
    }
    let duration = before.elapsed();

    for (local_root_span_id, endpoint_value) in std::mem::take(&mut replayer.endpoints) {
        outprof.add_endpoint(local_root_span_id, Cow::Borrowed(endpoint_value));
    }

    println!("Replaying sample took {} ms", duration.as_millis());

    if let Some(file) = output {
        println!("Writing out pprof to file {file}");
        let encoded = outprof.serialize(Some(replayer.start_time), Some(replayer.duration))?;
        std::fs::write(file, encoded.buffer)?;
    }

    Ok(())
}
