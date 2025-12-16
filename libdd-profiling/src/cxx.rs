// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! CXX bindings for profiling module - provides a safe and idiomatic C++ API

#![allow(clippy::needless_lifetimes)]

use crate::api;
use crate::exporter;
use crate::internal;

// ============================================================================
// CXX Bridge - C++ Bindings
// ============================================================================

#[cxx::bridge(namespace = "datadog::profiling")]
pub mod ffi {
    // Shared structs - CXX-friendly types
    struct ValueType<'a> {
        type_: &'a str,
        unit: &'a str,
    }

    struct Period<'a> {
        value_type: ValueType<'a>,
        value: i64,
    }

    struct Mapping<'a> {
        memory_start: u64,
        memory_limit: u64,
        file_offset: u64,
        filename: &'a str,
        build_id: &'a str,
    }

    struct Function<'a> {
        name: &'a str,
        system_name: &'a str,
        filename: &'a str,
    }

    struct Location<'a> {
        mapping: Mapping<'a>,
        function: Function<'a>,
        address: u64,
        line: i64,
    }

    struct Label<'a> {
        key: &'a str,
        str: &'a str,
        num: i64,
        num_unit: &'a str,
    }

    struct Sample<'a> {
        locations: Vec<Location<'a>>,
        values: Vec<i64>,
        labels: Vec<Label<'a>>,
    }

    struct Tag<'a> {
        key: &'a str,
        value: &'a str,
    }

    struct AttachmentFile<'a> {
        name: &'a str,
        data: &'a [u8],
    }

    // Opaque Rust types
    extern "Rust" {
        type Profile;
        type ProfileExporter;

        // Static factory methods for Profile
        #[Self = "Profile"]
        fn create(sample_types: Vec<ValueType>, period: &Period) -> Result<Box<Profile>>;

        // Profile methods
        fn add_sample(self: &mut Profile, sample: &Sample) -> Result<()>;
        fn add_endpoint(self: &mut Profile, local_root_span_id: u64, endpoint: &str) -> Result<()>;
        fn add_endpoint_count(self: &mut Profile, endpoint: &str, value: i64) -> Result<()>;

        // Upscaling rule methods (one for each variant)
        fn add_upscaling_rule_poisson(
            self: &mut Profile,
            offset_values: &[usize],
            label_name: &str,
            label_value: &str,
            sum_value_offset: usize,
            count_value_offset: usize,
            sampling_distance: u64,
        ) -> Result<()>;

        fn add_upscaling_rule_poisson_non_sample_type_count(
            self: &mut Profile,
            offset_values: &[usize],
            label_name: &str,
            label_value: &str,
            sum_value_offset: usize,
            count_value: u64,
            sampling_distance: u64,
        ) -> Result<()>;

        fn add_upscaling_rule_proportional(
            self: &mut Profile,
            offset_values: &[usize],
            label_name: &str,
            label_value: &str,
            scale: f64,
        ) -> Result<()>;

        fn reset(self: &mut Profile) -> Result<()>;
        fn serialize_to_vec(self: &mut Profile) -> Result<Vec<u8>>;

        // Static factory methods for ProfileExporter
        #[Self = "ProfileExporter"]
        fn create_agent_exporter(
            profiling_library_name: &str,
            profiling_library_version: &str,
            family: &str,
            tags: Vec<Tag>,
            agent_url: &str,
            timeout_ms: u64,
        ) -> Result<Box<ProfileExporter>>;

        #[Self = "ProfileExporter"]
        fn create_agentless_exporter(
            profiling_library_name: &str,
            profiling_library_version: &str,
            family: &str,
            tags: Vec<Tag>,
            site: &str,
            api_key: &str,
            timeout_ms: u64,
        ) -> Result<Box<ProfileExporter>>;

        // ProfileExporter methods
        /// Sends a profile to Datadog.
        ///
        /// # Arguments
        /// * `profile` - Profile to send (will be reset after sending)
        /// * `files_to_compress` - Additional files to compress and attach (e.g., heap dumps)
        /// * `additional_tags` - Per-profile tags (in addition to exporter-level tags)
        /// * `internal_metadata` - Internal metadata as JSON string (e.g., `{"key": "value"}`) See
        ///   Datadog-internal "RFC: Attaching internal metadata to pprof profiles" Pass empty
        ///   string "" if not needed
        /// * `process_tags` - Process-level tags as comma-separated string (e.g.,
        ///   "runtime:native,profiler_version:1.0") Pass empty string "" if not needed
        /// * `info` - System/environment info as JSON string (e.g., `{"os": "linux", "arch":
        ///   "x86_64"}`) See Datadog-internal "RFC: Pprof System Info Support" Pass empty string ""
        ///   if not needed
        fn send_profile(
            self: &ProfileExporter,
            profile: &mut Profile,
            files_to_compress: Vec<AttachmentFile>,
            additional_tags: Vec<Tag>,
            process_tags: &str,
            internal_metadata: &str,
            info: &str,
        ) -> Result<()>;
    }
}

// ============================================================================
// From Implementations - Convert CXX types to API types
// ============================================================================

impl<'a> From<&ffi::ValueType<'a>> for api::ValueType<'a> {
    fn from(vt: &ffi::ValueType<'a>) -> Self {
        api::ValueType::new(vt.type_, vt.unit)
    }
}

impl<'a> From<&ffi::Period<'a>> for api::Period<'a> {
    fn from(period: &ffi::Period<'a>) -> Self {
        api::Period {
            r#type: (&period.value_type).into(),
            value: period.value,
        }
    }
}

impl<'a> From<&ffi::Mapping<'a>> for api::Mapping<'a> {
    fn from(mapping: &ffi::Mapping<'a>) -> Self {
        api::Mapping {
            memory_start: mapping.memory_start,
            memory_limit: mapping.memory_limit,
            file_offset: mapping.file_offset,
            filename: mapping.filename,
            build_id: mapping.build_id,
        }
    }
}

impl<'a> From<&ffi::Function<'a>> for api::Function<'a> {
    fn from(func: &ffi::Function<'a>) -> Self {
        api::Function {
            name: func.name,
            system_name: func.system_name,
            filename: func.filename,
        }
    }
}

impl<'a> From<&ffi::Location<'a>> for api::Location<'a> {
    fn from(loc: &ffi::Location<'a>) -> Self {
        api::Location {
            mapping: (&loc.mapping).into(),
            function: (&loc.function).into(),
            address: loc.address,
            line: loc.line,
        }
    }
}

impl<'a> From<&ffi::Label<'a>> for api::Label<'a> {
    fn from(label: &ffi::Label<'a>) -> Self {
        api::Label {
            key: label.key,
            str: label.str,
            num: label.num,
            num_unit: label.num_unit,
        }
    }
}

impl<'a> From<&ffi::AttachmentFile<'a>> for exporter::File<'a> {
    fn from(file: &ffi::AttachmentFile<'a>) -> Self {
        exporter::File {
            name: file.name,
            bytes: file.data,
        }
    }
}

impl<'a> TryFrom<&ffi::Tag<'a>> for exporter::Tag {
    type Error = anyhow::Error;

    fn try_from(tag: &ffi::Tag<'a>) -> Result<Self, Self::Error> {
        exporter::Tag::new(tag.key, tag.value)
    }
}

// ============================================================================
// Profile - Wrapper around internal::Profile
// ============================================================================

pub struct Profile {
    inner: internal::Profile,
}

impl Profile {
    pub fn create(
        sample_types: Vec<ffi::ValueType>,
        period: &ffi::Period,
    ) -> anyhow::Result<Box<Profile>> {
        // Convert using From trait
        let types: Vec<api::ValueType> = sample_types.iter().map(Into::into).collect();
        let period_value: api::Period = period.into();

        // Profile::try_new interns the strings
        let inner = internal::Profile::try_new(&types, Some(period_value))?;

        Ok(Box::new(Profile { inner }))
    }

    pub fn add_sample(&mut self, sample: &ffi::Sample) -> anyhow::Result<()> {
        let api_sample = api::Sample {
            locations: sample.locations.iter().map(Into::into).collect(),
            values: &sample.values,
            labels: sample.labels.iter().map(Into::into).collect(),
        };

        // Profile interns the strings
        self.inner.try_add_sample(api_sample, None)?;
        Ok(())
    }

    pub fn add_endpoint(&mut self, local_root_span_id: u64, endpoint: &str) -> anyhow::Result<()> {
        self.inner
            .add_endpoint(local_root_span_id, std::borrow::Cow::Borrowed(endpoint))
    }

    pub fn add_endpoint_count(&mut self, endpoint: &str, value: i64) -> anyhow::Result<()> {
        self.inner
            .add_endpoint_count(std::borrow::Cow::Borrowed(endpoint), value)
    }

    pub fn add_upscaling_rule_poisson(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        sum_value_offset: usize,
        count_value_offset: usize,
        sampling_distance: u64,
    ) -> anyhow::Result<()> {
        let upscaling_info = api::UpscalingInfo::Poisson {
            sum_value_offset,
            count_value_offset,
            sampling_distance,
        };
        self.inner
            .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info)
    }

    pub fn add_upscaling_rule_poisson_non_sample_type_count(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        sum_value_offset: usize,
        count_value: u64,
        sampling_distance: u64,
    ) -> anyhow::Result<()> {
        let upscaling_info = api::UpscalingInfo::PoissonNonSampleTypeCount {
            sum_value_offset,
            count_value,
            sampling_distance,
        };
        self.inner
            .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info)
    }

    pub fn add_upscaling_rule_proportional(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        scale: f64,
    ) -> anyhow::Result<()> {
        let upscaling_info = api::UpscalingInfo::Proportional { scale };
        self.inner
            .add_upscaling_rule(offset_values, label_name, label_value, upscaling_info)
    }

    pub fn reset(&mut self) -> anyhow::Result<()> {
        // Reset and discard the old profile
        self.inner.reset_and_return_previous()?;
        Ok(())
    }

    pub fn serialize_to_vec(&mut self) -> anyhow::Result<Vec<u8>> {
        // Reset the profile and get the old one to serialize
        let old_profile = self.inner.reset_and_return_previous()?;
        let end_time = Some(std::time::SystemTime::now());
        let encoded = old_profile.serialize_into_compressed_pprof(end_time, None)?;
        Ok(encoded.buffer)
    }
}

// ============================================================================
// ProfileExporter - Wrapper around exporter::ProfileExporter
// ============================================================================

pub struct ProfileExporter {
    inner: exporter::ProfileExporter,
}

impl ProfileExporter {
    pub fn create_agent_exporter(
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        tags: Vec<ffi::Tag>,
        agent_url: &str,
        timeout_ms: u64,
    ) -> anyhow::Result<Box<ProfileExporter>> {
        let mut endpoint = exporter::config::agent(agent_url.parse()?)?;

        // Set timeout if non-zero (0 means use default)
        if timeout_ms > 0 {
            endpoint.timeout_ms = timeout_ms;
        }

        let tags_vec: Vec<exporter::Tag> = tags
            .iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;

        let tags_option = if tags_vec.is_empty() {
            None
        } else {
            Some(tags_vec)
        };

        let inner = exporter::ProfileExporter::new(
            profiling_library_name.to_string(),
            profiling_library_version.to_string(),
            family.to_string(),
            tags_option,
            endpoint,
        )?;

        Ok(Box::new(ProfileExporter { inner }))
    }

    pub fn create_agentless_exporter(
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        tags: Vec<ffi::Tag>,
        site: &str,
        api_key: &str,
        timeout_ms: u64,
    ) -> anyhow::Result<Box<ProfileExporter>> {
        let mut endpoint = exporter::config::agentless(site, api_key.to_string())?;

        // Set timeout if non-zero (0 means use default)
        if timeout_ms > 0 {
            endpoint.timeout_ms = timeout_ms;
        }

        let tags_vec: Vec<exporter::Tag> = tags
            .iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;

        let tags_option = if tags_vec.is_empty() {
            None
        } else {
            Some(tags_vec)
        };

        let inner = exporter::ProfileExporter::new(
            profiling_library_name.to_string(),
            profiling_library_version.to_string(),
            family.to_string(),
            tags_option,
            endpoint,
        )?;

        Ok(Box::new(ProfileExporter { inner }))
    }

    /// Sends a profile to Datadog.
    ///
    /// # Arguments
    /// * `profile` - Profile to send (will be reset after sending)
    /// * `files_to_compress` - Additional files to compress and attach
    /// * `additional_tags` - Per-profile tags (in addition to exporter-level tags)
    /// * `internal_metadata` - Internal metadata as JSON string. Empty string if not needed.
    ///   Example: `{"custom_field": "value", "version": "1.0"}`
    /// * `info` - System/environment info as JSON string. Empty string if not needed. Example:
    ///   `{"os": "linux", "arch": "x86_64", "kernel": "5.15.0"}`
    pub fn send_profile(
        &self,
        profile: &mut Profile,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
    ) -> anyhow::Result<()> {
        // Reset the profile and get the old one to export
        let old_profile = profile.inner.reset_and_return_previous()?;
        let end_time = Some(std::time::SystemTime::now());
        let encoded = old_profile.serialize_into_compressed_pprof(end_time, None)?;

        // Convert attachment files to exporter::File
        let files_to_compress_vec: Vec<exporter::File> =
            files_to_compress.iter().map(Into::into).collect();

        // Convert additional tags
        let additional_tags_vec: Option<Vec<exporter::Tag>> = if additional_tags.is_empty() {
            None
        } else {
            Some(
                additional_tags
                    .iter()
                    .map(TryInto::try_into)
                    .collect::<Result<Vec<_>, _>>()?,
            )
        };

        // Parse JSON strings if provided
        let internal_metadata_json = if internal_metadata.is_empty() {
            None
        } else {
            Some(serde_json::from_str(internal_metadata)?)
        };

        let info_json = if info.is_empty() {
            None
        } else {
            Some(serde_json::from_str(info)?)
        };

        // Build and send the request
        let process_tags_opt = if process_tags.is_empty() {
            None
        } else {
            Some(process_tags)
        };

        let request = self.inner.build(
            encoded,
            &files_to_compress_vec,
            &[], // files_to_export_unmodified - empty
            additional_tags_vec.as_ref(),
            process_tags_opt,
            internal_metadata_json,
            info_json,
        )?;
        let response = self.inner.send(request, None)?;

        // Check response status
        if !response.status().is_success() {
            anyhow::bail!("Failed to export profile: HTTP {}", response.status());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_LIB_NAME: &str = "dd-trace-test";
    const TEST_LIB_VERSION: &str = "1.0.0";
    const TEST_FAMILY: &str = "test";

    fn create_test_value_type() -> ffi::ValueType<'static> {
        ffi::ValueType {
            type_: "wall-time",
            unit: "nanoseconds",
        }
    }

    fn create_test_profile() -> Box<Profile> {
        let wall_time = create_test_value_type();
        let period = ffi::Period {
            value_type: wall_time,
            value: 60,
        };
        Profile::create(vec![create_test_value_type()], &period).unwrap()
    }

    fn create_test_location(address: u64, line: i64) -> ffi::Location<'static> {
        ffi::Location {
            mapping: ffi::Mapping {
                memory_start: address & 0xFFFF0000,
                memory_limit: (address & 0xFFFF0000) + 0x10000000,
                file_offset: 0,
                filename: "/usr/lib/libtest.so",
                build_id: "abc123",
            },
            function: ffi::Function {
                name: "test_function",
                system_name: "_Z13test_functionv",
                filename: "/src/test.cpp",
            },
            address,
            line,
        }
    }

    fn create_test_sample() -> ffi::Sample<'static> {
        ffi::Sample {
            locations: vec![create_test_location(0x10003000, 100)],
            values: vec![1000000],
            labels: vec![],
        }
    }

    fn create_test_exporter() -> Box<ProfileExporter> {
        ProfileExporter::create_agent_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            TEST_FAMILY,
            vec![ffi::Tag {
                key: "env",
                value: "test",
            }],
            "http://localhost:1", // Port 1 unlikely to have server
            100,
        )
        .unwrap()
    }

    #[test]
    fn test_profile_operations() {
        let mut profile = create_test_profile();

        // Verify profile starts empty
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            0,
            "Profile should start with no samples"
        );

        // Add samples and verify they're tracked
        let sample = create_test_sample();
        profile.add_sample(&sample).unwrap();
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            1,
            "Profile should have 1 sample after adding"
        );

        // Add another sample with different address
        let sample2 = ffi::Sample {
            locations: vec![create_test_location(0x20003000, 200)],
            values: vec![2000000],
            labels: vec![],
        };
        profile.add_sample(&sample2).unwrap();
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            2,
            "Profile should have 2 samples"
        );

        // Test endpoints
        profile.add_endpoint(12345, "/api/test").unwrap();
        profile.add_endpoint(67890, "/api/other").unwrap();
        profile.add_endpoint_count("/api/test", 100).unwrap();

        // Test upscaling rules (verify they don't error)
        profile
            .add_upscaling_rule_poisson(&[0], "thread_id", "0", 0, 0, 1000000)
            .unwrap();
        profile
            .add_upscaling_rule_proportional(&[0], "thread_id", "1", 100.0)
            .unwrap();
        profile
            .add_upscaling_rule_poisson_non_sample_type_count(
                &[0],
                "thread_id",
                "2",
                0,
                50,
                1000000,
            )
            .unwrap();

        // Serialize and verify output
        let serialized = profile.serialize_to_vec().unwrap();
        assert!(
            serialized.len() > 100,
            "Serialized profile should be non-trivial"
        );

        // Verify it's a valid pprof by checking for gzip/zstd magic bytes
        assert!(
            serialized.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) || // zstd magic
            serialized.starts_with(&[0x1f, 0x8b]), // gzip magic
            "Serialized profile should be compressed"
        );

        // After serialization (which resets), profile should be empty
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            0,
            "Profile should be empty after serialize_to_vec"
        );

        // Add sample and test explicit reset
        profile.add_sample(&sample).unwrap();
        assert_eq!(profile.inner.only_for_testing_num_aggregated_samples(), 1);
        profile.reset().unwrap();
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            0,
            "Profile should be empty after reset"
        );
    }

    #[test]
    fn test_exporter_create() {
        // Test agent exporter with default timeout
        assert!(ProfileExporter::create_agent_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            TEST_FAMILY,
            vec![ffi::Tag {
                key: "service",
                value: "test"
            }],
            "http://localhost:8126",
            0,
        )
        .is_ok());

        // Test with multiple tags and custom timeout
        assert!(ProfileExporter::create_agent_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            TEST_FAMILY,
            vec![
                ffi::Tag {
                    key: "service",
                    value: "my-service"
                },
                ffi::Tag {
                    key: "env",
                    value: "prod"
                },
                ffi::Tag {
                    key: "version",
                    value: "2.0"
                },
            ],
            "http://localhost:8126",
            10000,
        )
        .is_ok());

        // Test agentless exporters with different sites
        assert!(ProfileExporter::create_agentless_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            TEST_FAMILY,
            vec![],
            "datadoghq.com",
            "fake-api-key",
            5000,
        )
        .is_ok());

        assert!(ProfileExporter::create_agentless_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            TEST_FAMILY,
            vec![],
            "datadoghq.eu",
            "fake-api-key",
            0,
        )
        .is_ok());

        // Test with no tags
        assert!(ProfileExporter::create_agent_exporter(
            TEST_LIB_NAME,
            TEST_LIB_VERSION,
            TEST_FAMILY,
            vec![],
            "http://localhost:8126",
            0,
        )
        .is_ok());
    }

    #[test]
    fn test_type_conversions() {
        // AttachmentFile conversion
        let data = vec![1u8, 2, 3, 4, 5, 255, 128, 0];
        let file: exporter::File = (&ffi::AttachmentFile {
            name: "test.bin",
            data: &data,
        })
            .into();
        assert_eq!(file.name, "test.bin");
        assert_eq!(file.bytes, data.as_slice());

        // Tag conversion with special characters
        let tag: exporter::Tag = (&ffi::Tag {
            key: "test-key.with_special:chars",
            value: "test_value/with@special#chars",
        })
            .try_into()
            .unwrap();
        assert_eq!(
            tag.as_ref(),
            "test-key.with_special:chars:test_value/with@special#chars"
        );

        // Tag validation - empty key should fail
        assert!(TryInto::<exporter::Tag>::try_into(&ffi::Tag {
            key: "",
            value: "value"
        })
        .is_err());

        // ValueType conversion
        let vt: api::ValueType = (&ffi::ValueType {
            type_: "cpu-samples",
            unit: "count",
        })
            .into();
        assert_eq!(vt.r#type, "cpu-samples");
        assert_eq!(vt.unit, "count");

        // Mapping conversion
        let mapping: api::Mapping = (&ffi::Mapping {
            memory_start: 0x1000,
            memory_limit: 0x2000,
            file_offset: 0x100,
            filename: "/lib/test.so",
            build_id: "build123",
        })
            .into();
        assert_eq!(
            (
                mapping.memory_start,
                mapping.memory_limit,
                mapping.file_offset
            ),
            (0x1000, 0x2000, 0x100)
        );
        assert_eq!(
            (mapping.filename, mapping.build_id),
            ("/lib/test.so", "build123")
        );

        // Function conversion
        let function: api::Function = (&ffi::Function {
            name: "my_func",
            system_name: "_Z7my_funcv",
            filename: "/src/file.cpp",
        })
            .into();
        assert_eq!(
            (function.name, function.system_name, function.filename),
            ("my_func", "_Z7my_funcv", "/src/file.cpp")
        );

        // Label conversion
        let label: api::Label = (&ffi::Label {
            key: "thread_id",
            str: "",
            num: 42,
            num_unit: "thread",
        })
            .into();
        assert_eq!(
            (label.key, label.num, label.num_unit),
            ("thread_id", 42, "thread")
        );
    }

    #[test]
    fn test_send_profile_with_attachments() {
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample()).unwrap();

        let exporter = create_test_exporter();
        let attachment_data = br#"{"test": "data", "number": 123}"#.to_vec();

        // Send with full parameters - should fail with connection error but build request correctly
        let result = exporter.send_profile(
            &mut profile,
            vec![ffi::AttachmentFile {
                name: "metadata.json",
                data: &attachment_data,
            }],
            vec![
                ffi::Tag {
                    key: "profile_type",
                    value: "cpu",
                },
                ffi::Tag {
                    key: "runtime",
                    value: "native",
                },
            ],
            "language:rust,profiler_version:1.0",
            r#"{"version": "1.0", "profiler": "test"}"#,
            r#"{"os": "linux", "arch": "x86_64", "cores": 8}"#,
        );

        assert!(result.is_err(), "Should fail when no server available");
        assert_eq!(
            profile.inner.only_for_testing_num_aggregated_samples(),
            0,
            "Profile should be reset after send attempt"
        );

        // Test with empty optional parameters
        profile.add_sample(&create_test_sample()).unwrap();
        let result2 = exporter.send_profile(&mut profile, vec![], vec![], "", "", "");
        assert!(
            result2.is_err(),
            "Should fail with empty optional params too"
        );
    }
}
