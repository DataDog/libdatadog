// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::cancellation::CancellationToken;
use super::errors::ProfileExporterResult;
use super::ffi;
use super::profile::{EncodedProfile, Profile};
use crate::exporter;
use crate::internal;

pub(super) type PreparedExportArgs<'a> = (
    Vec<exporter::File<'a>>,
    Vec<libdd_common::tag::Tag>,
    Option<&'a str>,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
);

pub(super) fn prepare_export_args<'a>(
    files_to_compress: Vec<ffi::AttachmentFile<'a>>,
    additional_tags: Vec<ffi::Tag>,
    process_tags: &'a str,
    internal_metadata: &str,
    info: &str,
) -> anyhow::Result<PreparedExportArgs<'a>> {
    let files_to_compress_vec: Vec<exporter::File> =
        files_to_compress.iter().map(Into::into).collect();

    let additional_tags_vec: Vec<libdd_common::tag::Tag> = additional_tags
        .iter()
        .map(TryInto::try_into)
        .collect::<Result<Vec<_>, _>>()?;

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

    let process_tags_opt = if process_tags.is_empty() {
        None
    } else {
        Some(process_tags)
    };

    Ok((
        files_to_compress_vec,
        additional_tags_vec,
        process_tags_opt,
        internal_metadata_json,
        info_json,
    ))
}

/// Helper to encode a profile and prepare arguments for sending/queuing.
///
/// Resets the profile and returns the encoded previous profile data along with
/// converted arguments ready for the exporter APIs.
#[allow(clippy::type_complexity)]
pub(super) fn prepare_profile_for_export<'a>(
    profile: &mut Profile,
    files_to_compress: Vec<ffi::AttachmentFile<'a>>,
    additional_tags: Vec<ffi::Tag>,
    process_tags: &'a str,
    internal_metadata: &str,
    info: &str,
) -> anyhow::Result<(
    Box<EncodedProfile>,
    Vec<exporter::File<'a>>,
    Vec<libdd_common::tag::Tag>,
    Option<&'a str>,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
)> {
    let mut encoded_result = profile.serialize();
    anyhow::ensure!(encoded_result.ok(), encoded_result.status.message());
    let encoded = encoded_result.take();
    let (
        files_to_compress_vec,
        additional_tags_vec,
        process_tags_opt,
        internal_metadata_json,
        info_json,
    ) = prepare_export_args(
        files_to_compress,
        additional_tags,
        process_tags,
        internal_metadata,
        info,
    )?;

    Ok((
        encoded,
        files_to_compress_vec,
        additional_tags_vec,
        process_tags_opt,
        internal_metadata_json,
        info_json,
    ))
}

// ============================================================================
// ProfileExporter - Wrapper around exporter::ProfileExporter
// ============================================================================

pub struct ProfileExporter {
    pub(crate) inner: exporter::ProfileExporter,
}

impl ProfileExporter {
    pub fn create_agent_exporter(
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        tags: Vec<ffi::Tag>,
        agent_url: &str,
        timeout_ms: u64,
        use_system_resolver: bool,
    ) -> Box<ProfileExporterResult> {
        ProfileExporterResult::from_result(
            "ProfileExporter::create_agent_exporter",
            (|| -> anyhow::Result<Box<ProfileExporter>> {
                let mut endpoint = exporter::config::agent(agent_url.parse()?)?;

                // Set timeout if non-zero (0 means use default)
                if timeout_ms > 0 {
                    endpoint.timeout_ms = timeout_ms;
                }
                endpoint = endpoint.with_system_resolver(use_system_resolver);

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
            })(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_agentless_exporter(
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        tags: Vec<ffi::Tag>,
        site: &str,
        api_key: &str,
        timeout_ms: u64,
        use_system_resolver: bool,
    ) -> Box<ProfileExporterResult> {
        ProfileExporterResult::from_result(
            "ProfileExporter::create_agentless_exporter",
            (|| -> anyhow::Result<Box<ProfileExporter>> {
                let mut endpoint = exporter::config::agentless(site, api_key.to_string())?;

                // Set timeout if non-zero (0 means use default)
                if timeout_ms > 0 {
                    endpoint.timeout_ms = timeout_ms;
                }
                endpoint = endpoint.with_system_resolver(use_system_resolver);

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
            })(),
        )
    }

    pub fn create_file_exporter(
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        tags: Vec<ffi::Tag>,
        output_path: &str,
    ) -> Box<ProfileExporterResult> {
        ProfileExporterResult::from_result(
            "ProfileExporter::create_file_exporter",
            (|| -> anyhow::Result<Box<ProfileExporter>> {
                let endpoint = exporter::config::file(output_path)?;

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
            })(),
        )
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
        &mut self,
        profile: &mut Profile,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
    ) -> ffi::Status {
        ffi::Status::from_result(
            "ProfileExporter::send_profile",
            self.send_profile_impl(
                profile,
                files_to_compress,
                additional_tags,
                process_tags,
                internal_metadata,
                info,
                None,
            ),
        )
    }

    /// Sends a profile to Datadog with cancellation support.
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
    ) -> ffi::Status {
        ffi::Status::from_result(
            "ProfileExporter::send_profile_with_cancellation",
            self.send_profile_impl(
                profile,
                files_to_compress,
                additional_tags,
                process_tags,
                internal_metadata,
                info,
                Some(&cancel.inner),
            ),
        )
    }

    /// Internal implementation shared by send_profile and send_profile_with_cancellation
    ///
    /// Resets the profile and sends the previous profile data. This allows continuous
    /// profiling where you keep adding samples to the current profile while the previous
    /// period's data is being sent.
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
        let (
            encoded,
            files_to_compress_vec,
            additional_tags_vec,
            process_tags_opt,
            internal_metadata_json,
            info_json,
        ) = prepare_profile_for_export(
            profile,
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
        )?;

        let EncodedProfile { inner: encoded } = *encoded;
        self.send_encoded_profile_prepared(
            encoded,
            &files_to_compress_vec,
            &additional_tags_vec,
            process_tags_opt,
            internal_metadata_json,
            info_json,
            cancel,
        )
    }

    /// Sends a previously serialized profile to Datadog.
    #[allow(clippy::boxed_local)]
    pub fn send_encoded_profile(
        &mut self,
        encoded: Box<EncodedProfile>,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
    ) -> ffi::Status {
        ffi::Status::from_result(
            "ProfileExporter::send_encoded_profile",
            self.send_encoded_profile_impl(
                encoded,
                files_to_compress,
                additional_tags,
                process_tags,
                internal_metadata,
                info,
                None,
            ),
        )
    }

    /// Sends a previously serialized profile to Datadog with cancellation support.
    #[allow(clippy::boxed_local, clippy::too_many_arguments)]
    pub fn send_encoded_profile_with_cancellation(
        &mut self,
        encoded: Box<EncodedProfile>,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
        cancel: &CancellationToken,
    ) -> ffi::Status {
        ffi::Status::from_result(
            "ProfileExporter::send_encoded_profile_with_cancellation",
            self.send_encoded_profile_impl(
                encoded,
                files_to_compress,
                additional_tags,
                process_tags,
                internal_metadata,
                info,
                Some(&cancel.inner),
            ),
        )
    }

    #[allow(clippy::boxed_local, clippy::too_many_arguments)]
    fn send_encoded_profile_impl(
        &mut self,
        encoded: Box<EncodedProfile>,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &str,
        internal_metadata: &str,
        info: &str,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> anyhow::Result<()> {
        let (
            files_to_compress_vec,
            additional_tags_vec,
            process_tags_opt,
            internal_metadata_json,
            info_json,
        ) = prepare_export_args(
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
        )?;

        let EncodedProfile { inner: encoded } = *encoded;
        self.send_encoded_profile_prepared(
            encoded,
            &files_to_compress_vec,
            &additional_tags_vec,
            process_tags_opt,
            internal_metadata_json,
            info_json,
            cancel,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn send_encoded_profile_prepared(
        &mut self,
        encoded: internal::EncodedProfile,
        files_to_compress: &[exporter::File<'_>],
        additional_tags: &[libdd_common::tag::Tag],
        process_tags: Option<&str>,
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> anyhow::Result<()> {
        let status = self.inner.send_blocking(
            encoded,
            files_to_compress,
            additional_tags,
            internal_metadata,
            info,
            process_tags,
            cancel,
        )?;

        anyhow::ensure!(
            status.is_success(),
            "Failed to export profile: HTTP {status}",
        );

        Ok(())
    }
}
