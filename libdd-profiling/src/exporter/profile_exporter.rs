// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! ureq-based profiling exporter.
//!
//! ## Debugging with File Dumps
//!
//! For debugging and testing purposes, you can configure the exporter to dump
//! the raw HTTP requests to a file by using a `file://` URL:
//!
//! ```no_run
//! use libdd_profiling::exporter::{config, ProfileExporter};
//!
//! # fn main() -> anyhow::Result<()> {
//! let endpoint = config::file("/tmp/profile_dump.http")?;
//! let exporter = ProfileExporter::new("dd-trace-test", "1.0.0", "rust", vec![], endpoint)?;
//! // Requests will be saved to /tmp/profile_dump.http (overwrites on multiple sends)
//! # Ok(())
//! # }
//! ```
//!
//! The dumped files contain the complete HTTP request including headers and body,
//! which can be useful for debugging or replaying requests.

use super::errors::SendError;
use super::multipart::build_multipart;
use super::transport::{PreparedRequest, ProfileTransport};
use libdd_common::tag::Tag;
use libdd_common::{azure_app_services, tag, Endpoint};
use serde_json::json;

use crate::internal::EncodedProfile;

#[derive(Clone, Debug)]
pub struct ProfileExporter {
    transport: ProfileTransport,
    family: String,
    base_tags_string: String,
    headers: Vec<(String, String)>,
}

pub struct File<'a> {
    pub name: &'a str,
    pub bytes: &'a [u8],
}

impl ProfileExporter {
    /// Creates a new exporter to be used to report profiling data.
    ///
    /// # Performance
    ///
    /// TLS configuration is cached globally and reused across exporter
    /// instances, avoiding repeated root store loading on Linux.
    pub fn new(
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        mut tags: Vec<Tag>,
        endpoint: Endpoint,
    ) -> anyhow::Result<Self> {
        // Pre-build all static headers.
        let mut headers = Vec::with_capacity(8);
        headers.push(("Connection".to_string(), "close".to_string()));
        headers.push((
            "DD-EVP-ORIGIN".to_string(),
            profiling_library_name.to_string(),
        ));
        headers.push((
            "DD-EVP-ORIGIN-VERSION".to_string(),
            profiling_library_version.to_string(),
        ));
        headers.push((
            "User-Agent".to_string(),
            format!("DDProf/{}", env!("CARGO_PKG_VERSION")),
        ));

        // Add optional endpoint headers (api-key, test-token).
        for (name, value) in endpoint.get_optional_headers() {
            headers.push((name.to_string(), value.to_string()));
        }

        // Add entity-related headers (container-id, entity-id, external-env).
        for (name, value) in libdd_common::entity_id::get_entity_headers() {
            headers.push((name.to_string(), value.to_string()));
        }

        // Add Azure App Services tags if available.
        if let Some(aas) = &*azure_app_services::AAS_METADATA {
            let aas_tags_iter = aas.get_app_service_tags();
            tags.try_reserve(aas_tags_iter.len())?;
            tags.extend(aas_tags_iter.filter_map(|(name, value)| Tag::new(name, value).ok()));
        }

        // Precompute the base tags string (includes configured tags + Azure App Services tags).
        let base_tags_string: String = tags.iter().flat_map(|tag| [tag.as_ref(), ","]).collect();

        let resolved = endpoint.resolve_for_http()?;
        let tls_config = resolved
            .request_url
            .starts_with("https://")
            .then(|| super::tls::cached_tls_config().map(|config| config.0))
            .transpose()?;
        let transport = ProfileTransport::new(resolved, tls_config)?;

        Ok(Self {
            transport,
            family: family.to_string(),
            base_tags_string,
            headers,
        })
    }

    /// Synchronously sends a profile to the configured endpoint.
    ///
    /// This remains purely blocking so the exporter can stay runtime-free and
    /// statically linkable without pulling in Tokio.
    ///
    #[allow(clippy::too_many_arguments)]
    pub fn send_blocking(
        &mut self,
        profile: EncodedProfile,
        additional_files: &[File<'_>],
        additional_tags: &[Tag],
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
        process_tags: Option<&str>,
    ) -> anyhow::Result<http::StatusCode> {
        self.send_status(
            profile,
            additional_files,
            additional_tags,
            internal_metadata,
            info,
            process_tags,
        )
        .map_err(anyhow::Error::from)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build(
        &self,
        profile: EncodedProfile,
        additional_files: &[File<'_>],
        additional_tags: &[Tag],
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
        process_tags: Option<&str>,
    ) -> Result<PreparedRequest, SendError> {
        let tags_profiler = self.build_tags_string(additional_tags)?;
        let event = self.build_event_json(
            &profile,
            additional_files,
            &tags_profiler,
            internal_metadata,
            info,
            process_tags,
        );

        let multipart =
            build_multipart(&event, profile, additional_files).map_err(SendError::BuildFailed)?;

        let mut headers = self.headers.clone();
        headers.push(("Content-Type".to_string(), multipart.content_type));
        headers.push((
            "Content-Length".to_string(),
            multipart.body.len().to_string(),
        ));

        Ok(PreparedRequest {
            headers,
            body: multipart.body,
        })
    }

    #[allow(clippy::too_many_arguments)]
    /// Build and send a profile. Returns the HTTP status code.
    fn send_status(
        &self,
        profile: EncodedProfile,
        additional_files: &[File<'_>],
        additional_tags: &[Tag],
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
        process_tags: Option<&str>,
    ) -> Result<http::StatusCode, SendError> {
        let request = self.build(
            profile,
            additional_files,
            additional_tags,
            internal_metadata,
            info,
            process_tags,
        )?;

        self.send_prepared(request)
            .map_err(SendError::RequestFailed)
    }

    pub(crate) fn send_prepared(
        &self,
        request: PreparedRequest,
    ) -> anyhow::Result<http::StatusCode> {
        self.transport.send(request)
    }

    fn build_tags_string(&self, additional_tags: &[Tag]) -> anyhow::Result<String> {
        // Start with precomputed base tags (includes configured tags + Azure App Services tags).
        let mut tags = self.base_tags_string.clone();

        // Add additional tags with try_reserve to avoid OOM.
        for tag in additional_tags {
            let t = tag.as_ref();
            tags.try_reserve(t.len() + ','.len_utf8())?;
            tags.push_str(t);
            tags.push(',');
        }

        // Add runtime platform tag last, without a trailing comma.
        let t = tag!("runtime_platform", target_triple::TARGET);
        tags.try_reserve_exact(t.as_ref().len())?;
        tags.push_str(t.as_ref());
        Ok(tags)
    }

    fn build_event_json(
        &self,
        profile: &EncodedProfile,
        additional_files: &[File<'_>],
        tags_profiler: &str,
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
        process_tags: Option<&str>,
    ) -> serde_json::Value {
        let attachments: Vec<_> = additional_files
            .iter()
            .map(|f| f.name)
            .chain(std::iter::once("profile.pprof"))
            .collect();

        let mut internal = internal_metadata.unwrap_or_else(|| json!({}));
        internal["libdatadog_version"] = json!(env!("CARGO_PKG_VERSION"));

        json!({
            "attachments": attachments,
            "tags_profiler": tags_profiler,
            "start": chrono::DateTime::<chrono::Utc>::from(profile.start)
                .format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string(),
            "end": chrono::DateTime::<chrono::Utc>::from(profile.end)
                .format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string(),
            "family": self.family,
            "version": "4",
            "endpoint_counts": (!profile.endpoints_stats.is_empty()).then_some(&profile.endpoints_stats),
            "process_tags": process_tags.filter(|s| !s.is_empty()),
            "internal": internal,
            "info": info.unwrap_or_else(|| json!({})),
        })
    }
}
