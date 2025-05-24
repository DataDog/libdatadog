// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

mod profile_index;
mod replayer;

use clap::{command, Arg, ArgAction};
use prost::Message;
use std::borrow::Cow;
use std::io::Cursor;
use std::time::Instant;
use sysinfo::{Pid, ProcessExt, RefreshKind, System, SystemExt};

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
                Some((values[midpoint] + values[midpoint + 1]) as f64 / 2.0)
            } else {
                Some(values[midpoint] as f64)
            }
        }
    }
}

/// Finds the Q1, Q2, Q3 values. Assumes the slice is sorted.
#[allow(clippy::unwrap_used)]
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

struct Sysinfo {
    pid: sysinfo::Pid,
    s: System,
    observations: Vec<(String, u64)>,
}

impl Sysinfo {
    pub fn new() -> Self {
        let pid = Pid::from(std::process::id() as usize);
        // TODO: only collect the stats we care about
        //let s = System::new_all();
        let s = System::new_with_specifics(RefreshKind::new().with_memory());
        let observations = vec![];
        Self {
            pid,
            s,
            observations,
        }
    }

    pub fn measure_memory(&mut self, label: &str) -> u64 {
        self.s.refresh_process(self.pid);
        #[allow(clippy::expect_used)]
        let process = self
            .s
            .process(self.pid)
            .expect("There to be memory info for our process");
        let m = process.memory();
        self.observations.push((label.to_string(), m));
        m
    }

    pub fn print_observations(&self) {
        let mut prev = None;
        println!("Memory usage (kB)");
        for (label, m) in &self.observations {
            if let Some(p) = prev {
                let delta = *m as i64 - p;
                println!("{}:\t{}\tDelta: {}", label, *m / 1000, delta / 1000);
            } else {
                println!("{}:\t{}", label, *m / 1000);
            }
            prev = Some(*m as i64);
        }
    }
}

#[allow(clippy::unwrap_used)]
fn main() -> anyhow::Result<()> {
    let matches = command!()
        .arg(
            Arg::new("input")
                .short('i')
                .help("the pprof to replay")
                .required(true),
        )
        .arg(
            Arg::new("mem")
                .short('m')
                .long("mem")
                .action(ArgAction::SetTrue)
                .help("collect memory statistics")
                .required(false),
        )
        .arg(
            Arg::new("print-samples")
                .long("print-samples")
                .help("verbose printing of the stacks")
                .required(false)
                .num_args(0),
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
    let print_stacks = matches.get_one("print-samples");
    let collect_memory_stats = matches.get_flag("mem");
    let mut sysinfo = if collect_memory_stats {
        Some(Sysinfo::new())
    } else {
        None
    };

    let source = {
        println!("Reading in pprof from file '{input}'");
        std::fs::read(input)?
    };

    let pprof = datadog_profiling_protobuf::prost_impls::Profile::decode(&mut Cursor::new(source))?;

    let mut replayer = Replayer::try_from(&pprof)?;

    let mut outprof =
        datadog_profiling::internal::Profile::new(&replayer.sample_types, replayer.period)
            .with_start_time(replayer.start_time)?;

    // Before benchmarking, let's calculate some statistics.
    // No point doing that if there aren't at least 4 samples though.
    let n_samples = replayer.samples.len();
    println!("Number of samples: {n_samples}.");
    if n_samples >= 4usize {
        let mut depths: Vec<usize> = replayer
            .samples
            .iter()
            .map(|(_, sample)| sample.locations.len())
            .collect();

        depths.sort();
        let min = depths.first().unwrap();
        let [q1, q2, q3] = quartiles(depths.as_slice()).unwrap();
        let max = depths.last().unwrap();

        println!("Min stack depth is {min}.");
        println!("Q1 = {q1}, Q2 = {q2}, Q3 = {q3}.");
        println!("Max stack depth is {max}.");
    }

    if let Some(true) = print_stacks {
        for (_tstamp, sample) in replayer.samples.iter() {
            sample.locations.iter().rev().for_each(|location| {
                let fname = location.function.name;
                let lineno = location.line;
                let filename = location.function.filename;
                println!("{fname}:{lineno} ({filename})");
            });
            println!();
        }
    }

    // When benchmarking, don't count the copying of the stacks, do that before.
    let samples = std::mem::take(&mut replayer.samples);

    if let Some(s) = &mut sysinfo {
        s.measure_memory("Before adding samples");
    }

    let before = Instant::now();
    for (timestamp, sample) in samples {
        outprof.add_sample(sample, timestamp)?;
    }
    let duration = before.elapsed();

    if let Some(s) = &mut sysinfo {
        s.measure_memory("After adding samples");
    }

    for (local_root_span_id, endpoint_value) in std::mem::take(&mut replayer.endpoints) {
        outprof.add_endpoint(local_root_span_id, Cow::Borrowed(endpoint_value))?;
    }

    println!("Replaying sample took {} ms", duration.as_millis());

    if let Some(file) = output {
        println!("Writing out pprof to file {file}");
        let encoded = outprof
            .serialize_into_compressed_pprof(Some(replayer.start_time), Some(replayer.duration))?;
        if let Some(s) = &mut sysinfo {
            s.measure_memory("After serializing");
        }

        std::fs::write(file, encoded.buffer)?;
    }

    if let Some(s) = &mut sysinfo {
        s.print_observations();
    }

    Ok(())
}
