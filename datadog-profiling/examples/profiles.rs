// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_profiling::api;
use datadog_profiling::internal::Profile;
use std::io::Write;
use std::process::exit;

// Keep this in-sync with profiles.c
fn main() {
    let walltime = api::ValueType::new("wall-time", "nanoseconds");
    let sample_types = [api::ValueType::new("samples", "count"), walltime];

    let period = api::Period {
        r#type: walltime,
        value: 10000,
    };

    let mapping = api::Mapping {
        filename: "/usr/local/bin/php",
        ..Default::default()
    };
    let sample = api::Sample {
        locations: vec![
            api::Location {
                mapping,
                function: api::Function {
                    name: "phpinfo",
                    filename: "/srv/public/index.php",
                    ..Default::default()
                },
                line: 3,
                ..Default::default()
            },
            api::Location {
                mapping,
                function: api::Function {
                    name: "{main}",
                    filename: "/srv/public/index.php",
                    ..Default::default()
                },
                ..Default::default()
            },
        ],
        values: &[1, 10000],
        labels: vec![],
    };

    // Intentionally use the current time.
    let mut profile = Profile::try_new(&sample_types, Some(period)).unwrap();

    match profile.try_add_sample(sample, None) {
        Ok(_) => {}
        Err(_) => exit(1),
    }

    match profile.serialize_into_compressed_pprof(None, None) {
        Ok(encoded_profile) => {
            let buffer = &encoded_profile.buffer;
            assert!(buffer.len() > 100);
            std::io::stdout()
                .write_all(buffer.as_slice())
                .expect("write to succeed");
        }
        Err(_) => exit(1),
    }
}
