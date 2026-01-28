// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use libdd_profiling::internal::EncodedProfile;
use serde_json::json;

#[cfg(test)]
mod tests {
    use super::*;
    use common::*;
    use libdd_common::tag;

    fn default_tags() -> Vec<libdd_common::tag::Tag> {
        vec![tag!("service", "php"), tag!("host", "bits")]
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn multipart_agent() {
        let (mut exporter, file_path) =
            create_file_exporter(TEST_LIB_NAME, TEST_LIB_VERSION, "php", default_tags(), None)
                .expect("exporter to construct");

        let additional_files = create_test_additional_files();
        let profile = EncodedProfile::test_instance().expect("test profile");

        exporter
            .send_blocking(profile, &additional_files, &[], None, None, None, None)
            .expect("send to succeed");

        // Parse request and validate
        let request = read_and_parse_request(&file_path).expect("parse HTTP request");
        let event_json = extract_event_json(&request).expect("extract event JSON");

        // Validate request line and headers
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/");
        assert!(!request.headers.contains_key("dd-api-key"));
        assert_eq!(request.headers.get("dd-evp-origin").unwrap(), TEST_LIB_NAME);
        assert_eq!(
            request.headers.get("dd-evp-origin-version").unwrap(),
            TEST_LIB_VERSION
        );

        // Validate event.json content
        assert_eq!(
            event_json["attachments"],
            json!(["jit.pprof", "metadata.json", "profile.pprof"])
        );
        assert_eq!(event_json["endpoint_counts"], json!(null));
        assert_eq!(event_json["family"], json!("php"));
        assert_eq!(
            event_json["internal"]["libdatadog_version"],
            json!(env!("CARGO_PKG_VERSION"))
        );

        // Validate tags
        let tags_profiler = event_json["tags_profiler"].as_str().unwrap();
        assert!(tags_profiler.contains("service:php"));
        assert!(tags_profiler.contains("host:bits"));

        let runtime_platform = tags_profiler
            .split(',')
            .find(|tag| tag.starts_with("runtime_platform:"))
            .expect("runtime_platform tag should exist");
        assert!(
            runtime_platform.starts_with(&format!("runtime_platform:{}", std::env::consts::ARCH)),
            "expected platform tag to start with runtime_platform:{} but got '{}'",
            std::env::consts::ARCH,
            runtime_platform
        );

        assert_eq!(event_json["version"], json!("4"));

        // Verify profile.pprof part exists with content
        let profile_part = request
            .multipart_parts
            .iter()
            .find(|p| p.name == "profile.pprof")
            .expect("profile.pprof part");
        assert!(
            !profile_part.content.is_empty(),
            "profile should have content"
        );

        // Verify all MIME types
        assert_all_standard_mime_types(&request.multipart_parts);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn including_internal_metadata() {
        let (mut exporter, file_path) =
            create_file_exporter(TEST_LIB_NAME, TEST_LIB_VERSION, "php", default_tags(), None)
                .expect("exporter to construct");

        let internal_metadata = json!({
            "no_signals_workaround_enabled": "true",
            "execution_trace_enabled": "false",
            "extra object": {"key": [1, 2, true]},
            "libdatadog_version": env!("CARGO_PKG_VERSION"),
        });

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

        let request = read_and_parse_request(&file_path).expect("parse HTTP request");
        let event_json = extract_event_json(&request).expect("extract event JSON");

        assert_eq!(event_json["internal"], internal_metadata);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn including_process_tags() {
        let (mut exporter, file_path) =
            create_file_exporter(TEST_LIB_NAME, TEST_LIB_VERSION, "php", default_tags(), None)
                .expect("exporter to construct");

        let expected_process_tags = "entrypoint.basedir:net10.0,entrypoint.name:buggybits.program,entrypoint.workdir:this_folder,runtime_platform:x86_64-pc-windows-msvc";

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

        let request = read_and_parse_request(&file_path).expect("parse HTTP request");
        let event_json = extract_event_json(&request).expect("extract event JSON");

        assert_eq!(event_json["process_tags"], expected_process_tags);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn including_info() {
        let (mut exporter, file_path) =
            create_file_exporter(TEST_LIB_NAME, TEST_LIB_VERSION, "php", default_tags(), None)
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

        let profile = EncodedProfile::test_instance().expect("test profile");
        exporter
            .send_blocking(profile, &[], &[], None, Some(info.clone()), None, None)
            .expect("send to succeed");

        let request = read_and_parse_request(&file_path).expect("parse HTTP request");
        let event_json = extract_event_json(&request).expect("extract event JSON");

        assert_eq!(event_json["info"], info);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn multipart_agentless() {
        let api_key = "1234567890123456789012";

        let (mut exporter, file_path) = create_file_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            "php",
            default_tags(),
            Some(api_key),
        )
        .expect("exporter to construct");

        let profile = EncodedProfile::test_instance().expect("test profile");
        exporter
            .send_blocking(profile, &[], &[], None, None, None, None)
            .expect("send to succeed");

        let request = read_and_parse_request(&file_path).expect("parse HTTP request");

        // Validate headers - API key should be present
        assert_eq!(request.headers.get("dd-api-key").unwrap(), api_key);
        assert_eq!(request.headers.get("dd-evp-origin").unwrap(), TEST_LIB_NAME);
        assert_eq!(
            request.headers.get("dd-evp-origin-version").unwrap(),
            TEST_LIB_VERSION
        );
    }
}
