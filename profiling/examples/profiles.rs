// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use datadog_profiling::profile::{api, Profile};
use std::io::Write;
use std::time::SystemTime;

/* The profile built doesn't match the same format as the PHP profiler, but
 * it is similar and should make sense.
 * Keep this in-sync with profiles.c
 */
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let start_time = SystemTime::now();

    let wall_time = api::ValueType {
        r#type: "wall-time",
        unit: "nanoseconds",
    };
    let cpu_time = api::ValueType {
        r#type: "cpu-time",
        unit: "nanoseconds",
    };
    let sample_types = vec![wall_time, cpu_time];

    let period = api::Period {
        r#type: wall_time,
        value: 60_000_000_000,
    };

    let ext_standard = api::Mapping {
        filename: "[ext/standard]",
        ..Default::default()
    };
    let mut sample = api::Sample {
        locations: vec![
            api::Location {
                mapping: ext_standard,
                lines: vec![api::Line {
                    function: api::Function {
                        name: "sleep",
                        ..Default::default()
                    },
                    line: 0,
                }],
                ..Default::default()
            },
            api::Location {
                mapping: ext_standard,
                lines: vec![api::Line {
                    function: api::Function {
                        name: "<?php",
                        filename: "/srv/example.org/index.php",
                        ..Default::default()
                    },
                    line: 3,
                }],
                ..Default::default()
            },
        ],
        values: vec![10_000, 73],
        labels: vec![api::Label {
            key: "process_id",
            str: Some("12345"),
            num: 0,
            num_unit: None,
        }],
        timestamp: SystemTime::now(),
    };

    let mut profile: Profile = Profile::builder()
        .sample_types(sample_types)
        .period(Some(period))
        .start_time(Some(start_time))
        .build();

    let sample_id1 = profile.add(sample.clone())?;

    sample.timestamp = SystemTime::now();
    let sample_id2 = profile.add(sample)?;

    assert_eq!(sample_id1, sample_id2, "Sample ids should match");

    let end_time = SystemTime::now();
    let encoded_profile = profile.serialize(Some(end_time), None)?;
    let buffer = &encoded_profile.buffer;
    std::io::stdout().write_all(buffer.as_slice())?;
    Ok(())
}
