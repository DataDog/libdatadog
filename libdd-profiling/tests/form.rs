// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_profiling::exporter::utils::{extract_boundary, parse_http_request, parse_multipart};
use libdd_profiling::exporter::ProfileExporter;
use libdd_profiling::internal::EncodedProfile;
use std::path::PathBuf;

/// Create a file-based exporter and return the temp file path
#[cfg(unix)]
fn create_file_exporter(
    profiling_library_name: &str,
    profiling_library_version: &str,
    family: &str,
    tags: Vec<libdd_common::tag::Tag>,
    api_key: Option<&str>,
) -> anyhow::Result<(ProfileExporter, PathBuf)> {
    use libdd_profiling::exporter::config;

    // Create a unique temp file path
    let temp_dir = std::env::temp_dir();
    let file_path = temp_dir.join(format!(
        "libdd_test_{}_{}.http",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));

    let mut endpoint = config::file(file_path.to_string_lossy().as_ref())?;
    if let Some(key) = api_key {
        endpoint.api_key = Some(key.to_string().into());
    }

    let exporter = ProfileExporter::new(
        profiling_library_name,
        profiling_library_version,
        family,
        tags,
        endpoint,
    )?;

    Ok((exporter, file_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_common::tag;
    use serde_json::json;

    fn default_tags() -> Vec<libdd_common::tag::Tag> {
        vec![tag!("service", "php"), tag!("host", "bits")]
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn multipart_agent() {
        let profiling_library_name = "dd-trace-foo";
        let profiling_library_version = "1.2.3";

        let (mut exporter, file_path) = create_file_exporter(
            profiling_library_name,
            profiling_library_version,
            "php",
            default_tags(),
            None,
        )
        .expect("exporter to construct");

        // Send profile
        let profile = EncodedProfile::test_instance().expect("test profile");
        exporter
            .send_blocking(profile, &[], &[], None, None, None, None)
            .expect("send to succeed");

        // Read the dump file (wait a moment for it to be written)
        std::thread::sleep(std::time::Duration::from_millis(200));
        let request_bytes = std::fs::read(&file_path).expect("read dump file");

        // Parse HTTP request
        let request = parse_http_request(&request_bytes).expect("parse HTTP request");

        // Validate request line
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/v1/input");

        // Validate headers
        assert!(!request.headers.contains_key("dd-api-key"));
        assert_eq!(
            request.headers.get("dd-evp-origin").unwrap(),
            profiling_library_name
        );
        assert_eq!(
            request.headers.get("dd-evp-origin-version").unwrap(),
            profiling_library_version
        );

        // Parse multipart body
        let content_type = request
            .headers
            .get("content-type")
            .expect("Content-Type header");
        let boundary = extract_boundary(content_type).expect("extract boundary");
        let parts = parse_multipart(&request.body, &boundary).expect("parse multipart");

        // Find event.json part
        let event_part = parts
            .iter()
            .find(|p| p.filename.as_deref() == Some("event.json"))
            .expect("event.json part");

        let event_json: serde_json::Value =
            serde_json::from_slice(&event_part.content).expect("parse event.json");

        // Validate event.json content
        assert_eq!(event_json["attachments"], json!(["profile.pprof"]));
        assert_eq!(event_json["endpoint_counts"], json!(null));
        assert_eq!(event_json["family"], json!("php"));
        assert_eq!(
            event_json["internal"]["libdatadog_version"],
            json!(env!("CARGO_PKG_VERSION"))
        );

        let tags_profiler = event_json["tags_profiler"]
            .as_str()
            .unwrap()
            .split(',')
            .collect::<Vec<_>>();
        assert!(tags_profiler.contains(&"service:php"));
        assert!(tags_profiler.contains(&"host:bits"));

        let runtime_platform = tags_profiler
            .iter()
            .find(|tag| tag.starts_with("runtime_platform:"))
            .expect("runtime_platform tag should exist");
        assert!(
            runtime_platform.starts_with(&format!("runtime_platform:{}", std::env::consts::ARCH)),
            "expected platform tag to start with runtime_platform:{} but got '{}'",
            std::env::consts::ARCH,
            runtime_platform
        );

        assert_eq!(event_json["version"], json!("4"));

        // Verify profile.pprof part exists
        let profile_part = parts
            .iter()
            .find(|p| p.name == "profile.pprof")
            .expect("profile.pprof part");
        assert!(
            !profile_part.content.is_empty(),
            "profile should have content"
        );

        // Clean up
        let _ = std::fs::remove_file(&file_path);
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn including_internal_metadata() {
        let profiling_library_name = "dd-trace-foo";
        let profiling_library_version = "1.2.3";

        let (mut exporter, file_path) = create_file_exporter(
            profiling_library_name,
            profiling_library_version,
            "php",
            default_tags(),
            None,
        )
        .expect("exporter to construct");

        let internal_metadata = json!({
            "no_signals_workaround_enabled": "true",
            "execution_trace_enabled": "false",
            "extra object": {"key": [1, 2, true]},
            "libdatadog_version": env!("CARGO_PKG_VERSION"),
        });

        // Send profile
        let profile = EncodedProfile::test_instance().expect("test profile");
        exporter
            .send_blocking(
                profile,
                &[],
                &[],
                Some(internal_metadata.clone()),
                None,
                None,
                None,
            )
            .expect("send to succeed");

        // Read the dump file (wait a moment for it to be written)
        std::thread::sleep(std::time::Duration::from_millis(200));
        let request_bytes = std::fs::read(&file_path).expect("read dump file");

        // Parse and validate
        let request = parse_http_request(&request_bytes).expect("parse HTTP request");
        let content_type = request.headers.get("content-type").expect("Content-Type");
        let boundary = extract_boundary(content_type).expect("extract boundary");
        let parts = parse_multipart(&request.body, &boundary).expect("parse multipart");

        let event_part = parts
            .iter()
            .find(|p| p.filename.as_deref() == Some("event.json"))
            .expect("event.json part");

        let event_json: serde_json::Value =
            serde_json::from_slice(&event_part.content).expect("parse event.json");

        assert_eq!(event_json["internal"], internal_metadata);

        // Clean up
        let _ = std::fs::remove_file(&file_path);
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn including_process_tags() {
        let profiling_library_name = "dd-trace-foo";
        let profiling_library_version = "1.2.3";

        let (mut exporter, file_path) = create_file_exporter(
            profiling_library_name,
            profiling_library_version,
            "php",
            default_tags(),
            None,
        )
        .expect("exporter to construct");

        let expected_process_tags = "entrypoint.basedir:net10.0,entrypoint.name:buggybits.program,entrypoint.workdir:this_folder,runtime_platform:x86_64-pc-windows-msvc";

        // Send profile
        let profile = EncodedProfile::test_instance().expect("test profile");
        exporter
            .send_blocking(
                profile,
                &[],
                &[],
                None,
                None,
                Some(expected_process_tags),
                None,
            )
            .expect("send to succeed");

        // Read the dump file (wait a moment for it to be written)
        std::thread::sleep(std::time::Duration::from_millis(200));
        let request_bytes = std::fs::read(&file_path).expect("read dump file");

        // Parse and validate
        let request = parse_http_request(&request_bytes).expect("parse HTTP request");
        let content_type = request.headers.get("content-type").expect("Content-Type");
        let boundary = extract_boundary(content_type).expect("extract boundary");
        let parts = parse_multipart(&request.body, &boundary).expect("parse multipart");

        let event_part = parts
            .iter()
            .find(|p| p.filename.as_deref() == Some("event.json"))
            .expect("event.json part");

        let event_json: serde_json::Value =
            serde_json::from_slice(&event_part.content).expect("parse event.json");

        assert_eq!(event_json["process_tags"], expected_process_tags);

        // Clean up
        let _ = std::fs::remove_file(&file_path);
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn including_info() {
        let profiling_library_name = "dd-trace-foo";
        let profiling_library_version = "1.2.3";

        let (mut exporter, file_path) = create_file_exporter(
            profiling_library_name,
            profiling_library_version,
            "php",
            default_tags(),
            None,
        )
        .expect("exporter to construct");

        let info = json!({
            "application": {
                "start_time": "2024-01-24T11:17:22+0000",
                "env": "test"
            },
            "runtime": {
                "engine": "ruby",
                "version": "3.2.0",
                "platform": "arm64-darwin22"
            },
            "profiler": {
                "version": "1.32.0",
                "libdatadog": "1.2.3-darwin",
                "settings": {}
            }
        });

        // Send profile
        let profile = EncodedProfile::test_instance().expect("test profile");
        exporter
            .send_blocking(profile, &[], &[], None, Some(info.clone()), None, None)
            .expect("send to succeed");

        // Read the dump file (wait a moment for it to be written)
        std::thread::sleep(std::time::Duration::from_millis(200));
        let request_bytes = std::fs::read(&file_path).expect("read dump file");

        // Parse and validate
        let request = parse_http_request(&request_bytes).expect("parse HTTP request");
        let content_type = request.headers.get("content-type").expect("Content-Type");
        let boundary = extract_boundary(content_type).expect("extract boundary");
        let parts = parse_multipart(&request.body, &boundary).expect("parse multipart");

        let event_part = parts
            .iter()
            .find(|p| p.filename.as_deref() == Some("event.json"))
            .expect("event.json part");

        let event_json: serde_json::Value =
            serde_json::from_slice(&event_part.content).expect("parse event.json");

        assert_eq!(event_json["info"], info);

        // Clean up
        let _ = std::fs::remove_file(&file_path);
    }

    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn multipart_agentless() {
        let profiling_library_name = "dd-trace-foo";
        let profiling_library_version = "1.2.3";
        let api_key = "1234567890123456789012";

        let (mut exporter, file_path) = create_file_exporter(
            profiling_library_name,
            profiling_library_version,
            "php",
            default_tags(),
            Some(api_key),
        )
        .expect("exporter to construct");

        // Send profile
        let profile = EncodedProfile::test_instance().expect("test profile");
        exporter
            .send_blocking(profile, &[], &[], None, None, None, None)
            .expect("send to succeed");

        // Read the dump file (wait a moment for it to be written)
        std::thread::sleep(std::time::Duration::from_millis(200));
        let request_bytes = std::fs::read(&file_path).expect("read dump file");

        // Parse HTTP request
        let request = parse_http_request(&request_bytes).expect("parse HTTP request");

        // Validate headers - API key should be present
        assert_eq!(request.headers.get("dd-api-key").unwrap(), api_key);
        assert_eq!(
            request.headers.get("dd-evp-origin").unwrap(),
            profiling_library_name
        );
        assert_eq!(
            request.headers.get("dd-evp-origin-version").unwrap(),
            profiling_library_version
        );

        // Clean up
        let _ = std::fs::remove_file(&file_path);
    }
}
