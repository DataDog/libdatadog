// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::cancellation::CancellationToken;
use super::errors::{Operation, ProfileExporterResult};
use super::ffi;
use super::profile::EncodedProfile;
use crate::exporter;
use crate::internal;
use libdd_common::Endpoint;

struct PreparedExportArgs<'a> {
    files_to_compress: Vec<exporter::File<'a>>,
    additional_tags: Vec<libdd_common::tag::Tag>,
    process_tags: Option<&'a str>,
    internal_metadata: Option<serde_json::Value>,
    info: Option<serde_json::Value>,
}

fn tags_from_cxx(tags: Vec<ffi::Tag>) -> anyhow::Result<Vec<libdd_common::tag::Tag>> {
    tags.iter().map(TryInto::try_into).collect()
}

fn optional_json(value: &str) -> anyhow::Result<Option<serde_json::Value>> {
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(serde_json::from_str(value)?))
    }
}

fn optional_str(value: &str) -> Option<&str> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn apply_timeout_and_resolver(
    mut endpoint: Endpoint,
    timeout_ms: u64,
    use_system_resolver: bool,
) -> Endpoint {
    // Set timeout if non-zero (0 means use default).
    if timeout_ms > 0 {
        endpoint.timeout_ms = timeout_ms;
    }
    endpoint.with_system_resolver(use_system_resolver)
}

fn prepare_export_args<'a>(
    files_to_compress: Vec<ffi::AttachmentFile<'a>>,
    additional_tags: Vec<ffi::Tag>,
    process_tags: &'a str,
    internal_metadata: &str,
    info: &str,
) -> anyhow::Result<PreparedExportArgs<'a>> {
    Ok(PreparedExportArgs {
        files_to_compress: files_to_compress.iter().map(Into::into).collect(),
        additional_tags: tags_from_cxx(additional_tags)?,
        process_tags: optional_str(process_tags),
        internal_metadata: optional_json(internal_metadata)?,
        info: optional_json(info)?,
    })
}

// ============================================================================
// ProfileExporter - Wrapper around exporter::ProfileExporter
// ============================================================================

pub struct ProfileExporter {
    pub(crate) inner: exporter::ProfileExporter,
}

impl ProfileExporter {
    fn from_endpoint(
        operation: Operation,
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        tags: Vec<ffi::Tag>,
        endpoint: anyhow::Result<Endpoint>,
    ) -> Box<ProfileExporterResult> {
        ProfileExporterResult::from_result(
            operation,
            (|| -> anyhow::Result<Box<ProfileExporter>> {
                let inner = exporter::ProfileExporter::new(
                    profiling_library_name,
                    profiling_library_version,
                    family,
                    tags_from_cxx(tags)?,
                    endpoint?,
                )?;
                Ok(Box::new(ProfileExporter { inner }))
            })(),
        )
    }

    pub fn create_agent_exporter(
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        tags: Vec<ffi::Tag>,
        agent_url: &str,
        timeout_ms: u64,
        use_system_resolver: bool,
    ) -> Box<ProfileExporterResult> {
        Self::from_endpoint(
            Operation::CreateAgentExporter,
            profiling_library_name,
            profiling_library_version,
            family,
            tags,
            (|| {
                let endpoint = exporter::config::agent(agent_url.parse()?)?;
                Ok(apply_timeout_and_resolver(
                    endpoint,
                    timeout_ms,
                    use_system_resolver,
                ))
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
        Self::from_endpoint(
            Operation::CreateAgentlessExporter,
            profiling_library_name,
            profiling_library_version,
            family,
            tags,
            exporter::config::agentless(site, api_key.to_string()).map(|endpoint| {
                apply_timeout_and_resolver(endpoint, timeout_ms, use_system_resolver)
            }),
        )
    }

    pub fn create_file_exporter(
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        tags: Vec<ffi::Tag>,
        output_path: &str,
    ) -> Box<ProfileExporterResult> {
        Self::from_endpoint(
            Operation::CreateFileExporter,
            profiling_library_name,
            profiling_library_version,
            family,
            tags,
            exporter::config::file(output_path),
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
            Operation::SendEncodedProfile,
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
            Operation::SendEncodedProfileWithCancellation,
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
        let args = prepare_export_args(
            files_to_compress,
            additional_tags,
            process_tags,
            internal_metadata,
            info,
        )?;
        let EncodedProfile { inner: encoded } = *encoded;
        self.send_encoded_profile_prepared(encoded, args, cancel)
    }

    fn send_encoded_profile_prepared(
        &mut self,
        encoded: internal::EncodedProfile,
        args: PreparedExportArgs<'_>,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> anyhow::Result<()> {
        let status = self.inner.send_blocking(
            encoded,
            &args.files_to_compress,
            &args.additional_tags,
            args.internal_metadata,
            args.info,
            args.process_tags,
            cancel,
        )?;

        anyhow::ensure!(
            status.is_success(),
            "Failed to export profile: HTTP {status}",
        );

        Ok(())
    }
}
