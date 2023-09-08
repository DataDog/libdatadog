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

fn multipart(exporter: &ProfileExporter, internal_metadata: Option<serde_json::Value>) -> Request {
    let small_pprof_name = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/profile.pprof");
    let buffer = open(small_pprof_name).expect("to open file and read its bytes");

    let files: &[File] = &[File {
        name: "profile.pprof",
        bytes: buffer.as_slice(),
        compress_before_exporting: true,
    }];

    let now = chrono::Utc::now();
    let start = now.sub(chrono::Duration::seconds(60));
    let end = now;

    let timeout = std::time::Duration::from_secs(10);

    let request = exporter
        .build(
            start,
            end,
            files,
            None,
            None,
            internal_metadata,
            timeout,
        )
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
    use serde_json::json;

    fn default_tags() -> Vec<Tag> {
        vec![
            Tag::new("service", "php").expect("static tags to be valid"),
            Tag::new("host", "bits").expect("static tags to be valid"),
        ]
    }

    fn parsed_event_json(request: Request) -> serde_json::Value {
        // Really hacky way of getting the event.json file contents, because I didn't want to implement a full multipart parser
        // and didn't find a particularly good alternative.
        // If you do figure out a better way, there's another copy of this code in the profiling-ffi tests, please update there too :)
        let body = request.body();
        let body_bytes: String = String::from_utf8_lossy(
            &futures::executor::block_on(hyper::body::to_bytes(body)).unwrap(),
        )
        .to_string();
        let event_json = body_bytes
            .lines()
            .skip_while(|line| !line.contains(r#"filename="event.json""#))
            .nth(2)
            .unwrap();

        serde_json::from_str(event_json).unwrap()
    }

    #[test]
    // This test invokes an external function SecTrustSettingsCopyCertificates
    // which Miri cannot evaluate.
    #[cfg_attr(miri, ignore)]
    fn multipart_agent() {
        let profiling_library_name = "dd-trace-foo";
        let profiling_library_version = "1.2.3";
        let base_url = "http://localhost:8126".parse().expect("url to parse");
        let endpoint = config::agent(base_url).expect("endpoint to construct");
        let exporter = ProfileExporter::new(
            profiling_library_name,
            profiling_library_version,
            "php",
            Some(default_tags()),
            endpoint,
        )
        .expect("exporter to construct");

        let request = multipart(&exporter, None);

        assert_eq!(
            request.uri().to_string(),
            "http://localhost:8126/profiling/v1/input"
        );

        let actual_headers = request.headers();
        assert!(!actual_headers.contains_key("DD-API-KEY"));
        assert_eq!(
            actual_headers.get("DD-EVP-ORIGIN").unwrap(),
            profiling_library_name
        );
        assert_eq!(
            actual_headers.get("DD-EVP-ORIGIN-VERSION").unwrap(),
            profiling_library_version
        );

        let parsed_event_json = parsed_event_json(request);

        assert_eq!(parsed_event_json["attachments"], json!(["profile.pprof"]));
        assert_eq!(parsed_event_json["endpoint_counts"], json!(null));
        assert_eq!(parsed_event_json["family"], json!("php"));
        assert_eq!(parsed_event_json["internal"], json!({}));
        assert_eq!(
            parsed_event_json["tags_profiler"],
            json!("service:php,host:bits")
        );
        assert_eq!(parsed_event_json["version"], json!("4"));
    }

    #[test]
    // This test invokes an external function SecTrustSettingsCopyCertificates
    // which Miri cannot evaluate.
    #[cfg_attr(miri, ignore)]
    fn including_internal_metadata() {
        let profiling_library_name = "dd-trace-foo";
        let profiling_library_version = "1.2.3";
        let base_url = "http://localhost:8126".parse().expect("url to parse");
        let endpoint = config::agent(base_url).expect("endpoint to construct");
        let exporter = ProfileExporter::new(
            profiling_library_name,
            profiling_library_version,
            "php",
            Some(default_tags()),
            endpoint,
        )
        .expect("exporter to construct");

        let internal_metadata = json!({
            "no_signals_workaround_enabled": "true",
            "execution_trace_enabled": "false",
            "extra object": {"key": [1, 2, true]}
        });
        let request = multipart(&exporter, Some(internal_metadata.clone()));
        let parsed_event_json = parsed_event_json(request);

        assert_eq!(parsed_event_json["internal"], internal_metadata);
    }

    #[test]
    // This test invokes an external function SecTrustSettingsCopyCertificates
    // which Miri cannot evaluate.
    #[cfg_attr(miri, ignore)]
    fn multipart_agentless() {
        let profiling_library_name = "dd-trace-foo";
        let profiling_library_version = "1.2.3";
        let api_key = "1234567890123456789012";
        let endpoint = config::agentless("datadoghq.com", api_key).expect("endpoint to construct");
        let exporter = ProfileExporter::new(
            profiling_library_name,
            profiling_library_version,
            "php",
            Some(default_tags()),
            endpoint,
        )
        .expect("exporter to construct");

        let request = multipart(&exporter, None);

        assert_eq!(
            request.uri().to_string(),
            "https://intake.profile.datadoghq.com/api/v2/profile"
        );

        let actual_headers = request.headers();

        assert_eq!(actual_headers.get("DD-API-KEY").unwrap(), api_key);

        assert_eq!(
            actual_headers.get("DD-EVP-ORIGIN").unwrap(),
            profiling_library_name
        );

        assert_eq!(
            actual_headers.get("DD-EVP-ORIGIN-VERSION").unwrap(),
            profiling_library_version
        );
    }
}
