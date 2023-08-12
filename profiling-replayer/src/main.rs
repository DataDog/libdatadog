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

/// Returns (_, true) if the midpoint is odd.
fn midpoint(values: &[usize]) -> Option<(usize, bool)> {
    if values.is_empty() {
        return None;
    }
    // 4 / = 2, but in offsets, 1 is the middle ([0, 1, 2, 3])
    let midoffset = values.len() / 2 - 1;
    // if it's odd, get the average of the two
    if (values.len() & 1usize) != 0 {
        Some((midoffset, true))
    } else {
        Some((midoffset, false))
    }
}

fn median(values: &[usize]) -> Option<f64> {
    match midpoint(values) {
        None => None,
        Some((midpoint, is_odd)) => {
            if is_odd {
                Some((values[midpoint] + values[midpoint + 1]) as f64)
            } else {
                Some(values[midpoint] as f64)
            }
        }
    }
}

/// Finds the Q1, Q2, Q3 values. Assumes the slice is sorted.
fn quartiles(values: &[usize]) -> Option<[f64; 3]> {
    // This calculates Q3 as the median of the values above the midpoint, which
    // depending on how you define Q3, this is possibly not correct.
    if values.len() < 4 {
        return None;
    }

    let (midpoint, _) = midpoint(values).unwrap();
    let q2 = median(values).unwrap();

    let q1 = median(&values[..midpoint]).unwrap();
    let q3 = median(&values[(midpoint + 1)..]).unwrap();

    Some([q1, q2, q3])
}

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

    // Before benchmarking, let's calculate some statistics.
    // No point doing that if there aren't at least 4 samples though.
    let n_samples = replayer.samples.len();
    println!("Number of samples: {n_samples}.");
    if n_samples >= 4usize {
        let mut depths: Vec<usize> = replayer
            .samples
            .iter()
            .map(|sample| sample.locations.len())
            .collect();

        depths.sort();
        let min = depths.first().unwrap();
        let [q1, q2, q3] = quartiles(depths.as_slice()).unwrap();
        let median = {};
        let max = depths.last().unwrap();

        let max = depths[depths.len() - 1];

        println!("Min stack depth is {min}.");
        println!("Q1 = {q1}, Q2 = {q2}, Q3 = {q3}.");
        println!("Max stack depth is {max}.");
    }

    // When benchmarking, don't count the copying of the stacks, do that before.
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
