// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use datadog_profiling::exporter::{File, ProfileExporter, Request};
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

fn multipart(exporter: &ProfileExporter) -> Request {
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
        .build(start, end, files, None, timeout, "dd-trace-foo", "1.2.3")
        .expect("request to be built");

    let actual_timeout = request.timeout().expect("timeout to exist");
    assert_eq!(actual_timeout, timeout);
    request
}

#[cfg(test)]
mod tests {
    use crate::multipart;
    use datadog_profiling::exporter::*;
    use ddcommon::tag::Tag;

    fn default_tags() -> Vec<Tag> {
        vec![
            Tag::new("service", "php").expect("static tags to be valid"),
            Tag::new("host", "bits").expect("static tags to be valid"),
        ]
    }

    #[test]
    fn multipart_agent() {
        let base_url = "http://localhost:8126".parse().expect("url to parse");
        let endpoint = config::agent(base_url).expect("endpoint to construct");
        let exporter = ProfileExporter::new("php", Some(default_tags()), endpoint)
            .expect("exporter to construct");

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
        let endpoint = config::agentless("datadoghq.com", api_key).expect("endpoint to construct");
        let exporter = ProfileExporter::new("php", Some(default_tags()), endpoint)
            .expect("exporter to construct");

        let request = multipart(&exporter);

        assert_eq!(
            request.uri().to_string(),
            "https://intake.profile.datadoghq.com/api/v2/profile"
        );

        let actual_headers = request.headers();

        assert_eq!(
            actual_headers.get("DD-API-KEY").expect("api key to exist"),
            api_key
        );
    }
}
