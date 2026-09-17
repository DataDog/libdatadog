// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::cancellation::CancellationToken;
use super::errors::{Operation, ProfileExporterResult};
use super::ffi;
use super::profile::EncodedProfile;
use crate::exporter;
use crate::internal;
use libdd_common::{parse_uri, Endpoint};

struct PreparedExportArgs<'a> {
    files: Vec<(String, &'a [u8])>,
    additional_tags: Vec<libdd_common::tag::Tag>,
    process_tags: Option<String>,
    internal_metadata: Option<serde_json::Value>,
    info: Option<serde_json::Value>,
}

fn tags_from_cxx(tags: Vec<ffi::Tag>) -> anyhow::Result<Vec<libdd_common::tag::Tag>> {
    tags.iter()
        .map(|tag| {
            let key = String::from_utf8_lossy(tag.key);
            let value = String::from_utf8_lossy(tag.value);
            libdd_common::tag::Tag::new(key.as_ref(), value.as_ref())
        })
        .collect()
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
    process_tags: &[u8],
    internal_metadata: &[u8],
    info: &[u8],
) -> anyhow::Result<PreparedExportArgs<'a>> {
    let process_tags_str = String::from_utf8_lossy(process_tags);
    let internal_metadata_str = String::from_utf8_lossy(internal_metadata);
    let info_str = String::from_utf8_lossy(info);
    Ok(PreparedExportArgs {
        files: files_to_compress
            .iter()
            .map(|f| (String::from_utf8_lossy(f.name).into_owned(), f.data))
            .collect(),
        additional_tags: tags_from_cxx(additional_tags)?,
        process_tags: optional_str(&process_tags_str).map(|s| s.to_string()),
        internal_metadata: optional_json(&internal_metadata_str)?,
        info: optional_json(&info_str)?,
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
        profiling_library_name: &[u8],
        profiling_library_version: &[u8],
        family: &[u8],
        tags: Vec<ffi::Tag>,
        agent_url: &[u8],
        timeout_ms: u64,
        use_system_resolver: bool,
    ) -> Box<ProfileExporterResult> {
        let name = String::from_utf8_lossy(profiling_library_name);
        let version = String::from_utf8_lossy(profiling_library_version);
        let fam = String::from_utf8_lossy(family);
        Self::from_endpoint(
            Operation::CreateAgentExporter,
            &name,
            &version,
            &fam,
            tags,
            (|| {
                let url = String::from_utf8_lossy(agent_url);
                let endpoint = exporter::config::agent(parse_uri(&url)?)?;
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
        profiling_library_name: &[u8],
        profiling_library_version: &[u8],
        family: &[u8],
        tags: Vec<ffi::Tag>,
        site: &[u8],
        api_key: &[u8],
        timeout_ms: u64,
        use_system_resolver: bool,
    ) -> Box<ProfileExporterResult> {
        let name = String::from_utf8_lossy(profiling_library_name);
        let version = String::from_utf8_lossy(profiling_library_version);
        let fam = String::from_utf8_lossy(family);
        let site = String::from_utf8_lossy(site);
        let api_key = String::from_utf8_lossy(api_key);
        Self::from_endpoint(
            Operation::CreateAgentlessExporter,
            &name,
            &version,
            &fam,
            tags,
            exporter::config::agentless(&*site, api_key.into_owned()).map(|endpoint| {
                apply_timeout_and_resolver(endpoint, timeout_ms, use_system_resolver)
            }),
        )
    }

    pub fn create_file_exporter(
        profiling_library_name: &[u8],
        profiling_library_version: &[u8],
        family: &[u8],
        tags: Vec<ffi::Tag>,
        output_path: &[u8],
    ) -> Box<ProfileExporterResult> {
        let name = String::from_utf8_lossy(profiling_library_name);
        let version = String::from_utf8_lossy(profiling_library_version);
        let fam = String::from_utf8_lossy(family);
        let path = String::from_utf8_lossy(output_path);
        Self::from_endpoint(
            Operation::CreateFileExporter,
            &name,
            &version,
            &fam,
            tags,
            exporter::config::file(&*path),
        )
    }

    /// Sends a previously serialized profile to Datadog.
    #[allow(clippy::boxed_local)]
    pub fn send_encoded_profile(
        &mut self,
        encoded: Box<EncodedProfile>,
        files_to_compress: Vec<ffi::AttachmentFile>,
        additional_tags: Vec<ffi::Tag>,
        process_tags: &[u8],
        internal_metadata: &[u8],
        info: &[u8],
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
        process_tags: &[u8],
        internal_metadata: &[u8],
        info: &[u8],
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
        process_tags: &[u8],
        internal_metadata: &[u8],
        info: &[u8],
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
        let files: Vec<_> = args
            .files
            .iter()
            .map(|(name, data)| exporter::File {
                name: name.as_str(),
                bytes: data,
            })
            .collect();
        let status = self.inner.send_blocking(
            encoded,
            &files,
            &args.additional_tags,
            args.internal_metadata,
            args.info,
            args.process_tags.as_deref(),
            cancel,
        )?;

        anyhow::ensure!(
            status.is_success(),
            "Failed to export profile: HTTP {status}",
        );

        Ok(())
    }
}
