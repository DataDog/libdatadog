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
    pub struct ValueType<'a> {
        pub type_: &'a str,
        pub unit: &'a str,
    }

    pub struct Period<'a> {
        pub value_type: ValueType<'a>,
        pub value: i64,
    }

    pub struct Mapping<'a> {
        pub memory_start: u64,
        pub memory_limit: u64,
        pub file_offset: u64,
        pub filename: &'a str,
        pub build_id: &'a str,
    }

    pub struct Function<'a> {
        pub name: &'a str,
        pub system_name: &'a str,
        pub filename: &'a str,
    }

    pub struct Location<'a> {
        pub mapping: Mapping<'a>,
        pub function: Function<'a>,
        pub address: u64,
        pub line: i64,
    }

    pub struct Label<'a> {
        pub key: &'a str,
        pub str: &'a str,
        pub num: i64,
        pub num_unit: &'a str,
    }

    pub struct Sample<'a> {
        pub locations: Vec<Location<'a>>,
        pub values: Vec<i64>,
        pub labels: Vec<Label<'a>>,
    }

    pub struct Tag<'a> {
        pub key: &'a str,
        pub value: &'a str,
    }

    pub struct AttachmentFile<'a> {
        pub name: &'a str,
        pub data: &'a [u8],
    }

    // Opaque Rust types
    extern "Rust" {
        type Profile;
        type ProfileExporter;
        type CancellationToken;

        // CancellationToken factory and methods
        fn new_cancellation_token() -> Box<CancellationToken>;
        fn clone_token(self: &CancellationToken) -> Box<CancellationToken>;
        fn cancel(self: &CancellationToken);
        fn is_cancelled(self: &CancellationToken) -> bool;

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
        /// * `process_tags` - Process-level tags as comma-separated string (e.g.,
        ///   "runtime:native,profiler_version:1.0") Pass empty string "" if not needed
        /// * `internal_metadata` - Internal metadata as JSON string (e.g., `{"key": "value"}`) See
        ///   Datadog-internal "RFC: Attaching internal metadata to pprof profiles" Pass empty
        ///   string "" if not needed
        /// * `info` - System/environment info as JSON string (e.g., `{"os": "linux", "arch":
        ///   "x86_64"}`) See Datadog-internal "RFC: Pprof System Info Support" Pass empty string ""
        ///   if not needed
        #[allow(clippy::too_many_arguments)]
        fn send_profile(
            self: &mut ProfileExporter,
            profile: &mut Profile,
            files_to_compress: Vec<AttachmentFile>,
            additional_tags: Vec<Tag>,
            process_tags: &str,
            internal_metadata: &str,
            info: &str,
        ) -> Result<()>;

        /// Sends a profile to Datadog with cancellation support.
        ///
        /// This is the same as `send_profile`, but allows cancelling the operation from another
        /// thread using a cancellation token.
        ///
        /// # Arguments
        /// * `profile` - Profile to send (will be reset after sending)
        /// * `files_to_compress` - Additional files to compress and attach (e.g., heap dumps)
        /// * `additional_tags` - Per-profile tags (in addition to exporter-level tags)
        /// * `process_tags` - Process-level tags as comma-separated string (e.g.,
        ///   "runtime:native,profiler_version:1.0") Pass empty string "" if not needed
        /// * `internal_metadata` - Internal metadata as JSON string (e.g., `{"key": "value"}`) See
        ///   Datadog-internal "RFC: Attaching internal metadata to pprof profiles" Pass empty
        ///   string "" if not needed
        /// * `info` - System/environment info as JSON string (e.g., `{"os": "linux", "arch":
        ///   "x86_64"}`) See Datadog-internal "RFC: Pprof System Info Support" Pass empty string ""
        ///   if not needed
        /// * `cancel` - Cancellation token to cancel the send operation
        #[allow(clippy::too_many_arguments)]
        fn send_profile_with_cancellation(
            self: &mut ProfileExporter,
            profile: &mut Profile,
            files_to_compress: Vec<AttachmentFile>,
            additional_tags: Vec<Tag>,
            process_tags: &str,
            internal_metadata: &str,
            info: &str,
            cancel: &CancellationToken,
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

impl<'a> TryFrom<&ffi::Tag<'a>> for libdd_common::tag::Tag {
    type Error = anyhow::Error;

    fn try_from(tag: &ffi::Tag<'a>) -> Result<Self, Self::Error> {
        libdd_common::tag::Tag::new(tag.key, tag.value)
    }
}

// ============================================================================
// CancellationToken - Wrapper around tokio_util::sync::CancellationToken
// ============================================================================

pub struct CancellationToken {
    inner: tokio_util::sync::CancellationToken,
}

/// Creates a new cancellation token.
pub fn new_cancellation_token() -> Box<CancellationToken> {
    Box::new(CancellationToken {
        inner: tokio_util::sync::CancellationToken::new(),
    })
}

impl CancellationToken {
    /// Clones the cancellation token.
    ///
    /// A cloned token is connected to the original token - either can be used
    /// to cancel or check cancellation status. The useful part is that they have
    /// independent lifetimes and can be dropped separately.
    ///
    /// This is useful for multi-threaded scenarios where one thread performs the
    /// send operation while another thread can cancel it.
    pub fn clone_token(&self) -> Box<CancellationToken> {
        Box::new(CancellationToken {
            inner: self.inner.clone(),
        })
    }

    /// Cancels the token.
    ///
    /// Note that cancellation is a terminal state; calling cancel multiple times
    /// has no additional effect.
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// Returns true if the token has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
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

        let tags_vec: Vec<libdd_common::tag::Tag> = tags
            .iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;

        let inner = exporter::ProfileExporter::new(
            profiling_library_name,
            profiling_library_version,
            family,
            tags_vec,
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

        let tags_vec: Vec<libdd_common::tag::Tag> = tags
            .iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;

        let inner = exporter::ProfileExporter::new(
            profiling_library_name,
            profiling_library_version,
            family,
            tags_vec,
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
    /// * `process_tags` - Process-level tags as comma-separated string. Empty string if not needed.
    /// * `internal_metadata` - Internal metadata as JSON string. Empty string if not needed.
    ///   Example: `{"custom_field": "value", "version": "1.0"}`
    /// * `info` - System/environment info as JSON string. Empty string if not needed. Example:
    ///   `{"os": "linux", "arch": "x86_64", "kernel": "5.15.0"}`
    #[allow(clippy::too_many_arguments)]
    pub fn send_profile(
        &mut self,
        profile: &mut Profile,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
    ) -> anyhow::Result<()> {
        self.send_profile_impl(
            profile,
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
            None,
        )
    }

    /// Sends a profile to Datadog with cancellation support.
    ///
    /// This is the same as `send_profile`, but allows cancelling the operation from another
    /// thread using a cancellation token.
    ///
    /// # Arguments
    /// * `profile` - Profile to send (will be reset after sending)
    /// * `files_to_compress` - Additional files to compress and attach
    /// * `additional_tags` - Per-profile tags (in addition to exporter-level tags)
    /// * `process_tags` - Process-level tags as comma-separated string. Empty string if not needed.
    /// * `internal_metadata` - Internal metadata as JSON string. Empty string if not needed.
    ///   Example: `{"custom_field": "value", "version": "1.0"}`
    /// * `info` - System/environment info as JSON string. Empty string if not needed. Example:
    ///   `{"os": "linux", "arch": "x86_64", "kernel": "5.15.0"}`
    /// * `cancel` - Cancellation token to cancel the send operation
    #[allow(clippy::too_many_arguments)]
    pub fn send_profile_with_cancellation(
        &mut self,
        profile: &mut Profile,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        self.send_profile_impl(
            profile,
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
            Some(&cancel.inner),
        )
    }

    /// Internal implementation shared by send_profile and send_profile_with_cancellation
    #[allow(clippy::too_many_arguments)]
    fn send_profile_impl(
        &mut self,
        profile: &mut Profile,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> anyhow::Result<()> {
        // Reset the profile and get the old one to export
        let old_profile = profile.inner.reset_and_return_previous()?;
        let end_time = Some(std::time::SystemTime::now());
        let encoded = old_profile.serialize_into_compressed_pprof(end_time, None)?;

        // Convert attachment files to exporter::File
        let files_to_compress_vec: Vec<exporter::File> =
            files_to_compress.iter().map(Into::into).collect();

        // Convert additional tags
        let additional_tags_vec: Vec<libdd_common::tag::Tag> = additional_tags
            .iter()
            .map(TryInto::try_into)
            .collect::<Result<Vec<_>, _>>()?;

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

        // Parse process_tags if provided
        let process_tags_opt = if process_tags.is_empty() {
            None
        } else {
            Some(process_tags)
        };

        // Send the request with optional cancellation support
        let status = self.inner.send_blocking(
            encoded,
            &files_to_compress_vec,
            &additional_tags_vec,
            internal_metadata_json,
            info_json,
            process_tags_opt,
            cancel,
        )?;

        // Check response status
        if !status.is_success() {
            anyhow::bail!("Failed to export profile: HTTP {}", status);
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
        let tag: libdd_common::tag::Tag = (&ffi::Tag {
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
        assert!(TryInto::<libdd_common::tag::Tag>::try_into(&ffi::Tag {
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

        let mut exporter = create_test_exporter();
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

    #[test]
    fn test_cancellation_token() {
        // Create a cancellation token
        let token = new_cancellation_token();
        assert!(!token.is_cancelled(), "Token should start uncancelled");

        // Clone the token
        let token_clone = token.clone_token();
        assert!(
            !token_clone.is_cancelled(),
            "Cloned token should be uncancelled"
        );

        // Cancel the original token
        token.cancel();
        assert!(token.is_cancelled(), "Token should be cancelled");
        assert!(
            token_clone.is_cancelled(),
            "Cloned token should also be cancelled"
        );

        // Calling cancel again should be safe (no-op)
        token.cancel();
        assert!(token.is_cancelled(), "Token should still be cancelled");
    }

    #[test]
    fn test_send_profile_with_cancellation() {
        let mut profile = create_test_profile();
        profile.add_sample(&create_test_sample()).unwrap();

        let mut exporter = create_test_exporter();
        let cancel_token = new_cancellation_token();

        // Test sending with a non-cancelled token (should fail due to no server)
        let result = exporter.send_profile_with_cancellation(
            &mut profile,
            vec![],
            vec![],
            "",
            "",
            "",
            &cancel_token,
        );
        assert!(result.is_err(), "Should fail when no server available");

        // Test with a pre-cancelled token
        profile.add_sample(&create_test_sample()).unwrap();
        let cancel_token2 = new_cancellation_token();
        cancel_token2.cancel();

        let result2 = exporter.send_profile_with_cancellation(
            &mut profile,
            vec![],
            vec![],
            "",
            "",
            "",
            &cancel_token2,
        );
        // Should still fail, but for a different reason (cancelled or connection)
        assert!(result2.is_err());
    }
}
