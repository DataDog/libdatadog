// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use ddprof_exporter::{Endpoint, File, ProfileExporterV3, Request, Tag};
use std::borrow::Cow;
use std::error::Error;
use std::io::Read;
use std::ops::Sub;
use std::path::Path;

fn open<P: AsRef<Path>>(path: P) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    let mut buffer = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut buffer)?;

    Ok(buffer)
}

fn multipart(exporter: &ProfileExporterV3) -> Request {
    let small_pprof_name = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/profile.pprof");
    let buffer = open(small_pprof_name).expect("to open file and read its bytes");

    let files: &[File] = &[File {
        name: "profile.pprof",
        bytes: buffer.as_slice(),
    }];

    let now = chrono::Utc::now();
    let start = now.sub(chrono::Duration::seconds(60));
    let end = now;

    let timeout = std::time::Duration::from_secs(10);

    let request = exporter
        .build(start, end, files, &[], timeout)
        .expect("request to be built");

    let actual_timeout = request.timeout().expect("timeout to exist");
    assert_eq!(actual_timeout, timeout);
    request
}

fn default_tags() -> Vec<Tag> {
    vec![
        Tag {
            name: Cow::Borrowed("service"),
            value: Cow::Borrowed("php"),
        },
        Tag {
            name: Cow::Borrowed("host"),
            value: Cow::Borrowed("bits"),
        },
    ]
}

#[test]
fn multipart_agent() {
    let base_url = "http://localhost:8126".parse().expect("url to parse");
    let endpoint = Endpoint::agent(base_url).expect("endpoint to construct");
    let exporter =
        ProfileExporterV3::new("php", default_tags(), endpoint).expect("exporter to construct");

    let request = multipart(&exporter);

    assert_eq!(
        request.uri().to_string(),
        "http://localhost:8126/profiling/v1/input"
    );

    let actual_headers = request.headers();
    assert!(!actual_headers.contains_key("DD-API-KEY"));
}

#[test]
fn multipart_agentless() {
    let api_key = "1234567890123456789012";
    let endpoint = Endpoint::agentless("datadoghq.com", api_key).expect("endpoint to construct");
    let exporter =
        ProfileExporterV3::new("php", default_tags(), endpoint).expect("exporter to construct");

    let request = multipart(&exporter);

    assert_eq!(
        request.uri().to_string(),
        "https://intake.profile.datadoghq.com/v1/input"
    );

    let actual_headers = request.headers();

    assert_eq!(
        actual_headers.get("DD-API-KEY").expect("api key to exist"),
        api_key
    );
}
