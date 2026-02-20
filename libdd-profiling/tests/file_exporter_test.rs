// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use libdd_common::test_utils::{create_temp_file_path, parse_http_request_sync, TempFileGuard};
use libdd_profiling::exporter::ProfileExporter;
use libdd_profiling::internal::EncodedProfile;

/// Create a file-based exporter and return the temp file path with auto-cleanup
fn create_file_exporter(
    profiling_library_name: &str,
    profiling_library_version: &str,
    family: &str,
    tags: Vec<libdd_common::tag::Tag>,
    api_key: Option<&str>,
) -> anyhow::Result<(ProfileExporter, TempFileGuard)> {
    use libdd_profiling::exporter::config;

    // Create a unique temp file path
    let file_path = create_temp_file_path("libdd_profiling_test", "http");

    let mut endpoint = config::file(file_path.to_string_lossy().as_ref())?;
    if let Some(key) = api_key {
        endpoint.api_key = Some(key.to_string().into());
    }

    #[allow(deprecated)]
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

        // Build and send profile
        let profile = EncodedProfile::test_instance().expect("test profile");
        exporter
            .send_blocking(profile, &[], &[], None, None, None, None)
            .expect("send to succeed");

        // Read the dump file
        // send_blocking() blocks until the request completes and file is synced
        let request_bytes = std::fs::read(&file_path).expect("read dump file");

        // Parse HTTP request
        let request = parse_http_request_sync(&request_bytes).expect("parse HTTP request");

        // Validate request line
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/"); // File exporter uses root path

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

        // Get parsed multipart body and find event.json part
        let event_part = request
            .multipart_parts
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
        let profile_part = request
            .multipart_parts
            .iter()
            .find(|p| p.name == "profile.pprof")
            .expect("profile.pprof part");
        assert!(
            !profile_part.content.is_empty(),
            "profile should have content"
        );
    }

    #[test]
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

        // Build and send profile
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

        // Read the dump file
        // send_blocking() blocks until the request completes and file is synced
        let request_bytes = std::fs::read(&file_path).expect("read dump file");

        // Parse and validate
        let request = parse_http_request_sync(&request_bytes).expect("parse HTTP request");
        let event_part = request
            .multipart_parts
            .iter()
            .find(|p| p.filename.as_deref() == Some("event.json"))
            .expect("event.json part");

        let event_json: serde_json::Value =
            serde_json::from_slice(&event_part.content).expect("parse event.json");

        assert_eq!(event_json["internal"], internal_metadata);
    }

    #[test]
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

        // Build and send profile
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

        // Read the dump file
        // send_blocking() blocks until the request completes and file is synced
        let request_bytes = std::fs::read(&file_path).expect("read dump file");

        // Parse and validate
        let request = parse_http_request_sync(&request_bytes).expect("parse HTTP request");
        let event_part = request
            .multipart_parts
            .iter()
            .find(|p| p.filename.as_deref() == Some("event.json"))
            .expect("event.json part");

        let event_json: serde_json::Value =
            serde_json::from_slice(&event_part.content).expect("parse event.json");

        assert_eq!(event_json["process_tags"], expected_process_tags);
    }

    #[test]
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

        // Build and send profile
        let profile = EncodedProfile::test_instance().expect("test profile");
        exporter
            .send_blocking(profile, &[], &[], None, Some(info.clone()), None, None)
            .expect("send to succeed");

        // Read the dump file
        // send_blocking() blocks until the request completes and file is synced
        let request_bytes = std::fs::read(&file_path).expect("read dump file");

        // Parse and validate
        let request = parse_http_request_sync(&request_bytes).expect("parse HTTP request");
        let event_part = request
            .multipart_parts
            .iter()
            .find(|p| p.filename.as_deref() == Some("event.json"))
            .expect("event.json part");

        let event_json: serde_json::Value =
            serde_json::from_slice(&event_part.content).expect("parse event.json");

        assert_eq!(event_json["info"], info);
    }

    #[test]
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

        // Build and send profile
        let profile = EncodedProfile::test_instance().expect("test profile");
        exporter
            .send_blocking(profile, &[], &[], None, None, None, None)
            .expect("send to succeed");

        // Read the dump file
        // send_blocking() blocks until the request completes and file is synced
        let request_bytes = std::fs::read(&file_path).expect("read dump file");

        // Parse HTTP request
        let request = parse_http_request_sync(&request_bytes).expect("parse HTTP request");

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

        // Check for entity headers and validate their values match what libdd_common provides
        common::assert_entity_headers_match(&request.headers);
    }
}
