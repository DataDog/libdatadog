// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Reqwest-based profiling exporter
//!
//! This is a simplified async implementation using reqwest.
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
use super::file_exporter::spawn_dump_server;
use anyhow::Context;
use libdd_common::tag::Tag;
use libdd_common::{azure_app_services, tag, Endpoint};
use reqwest::RequestBuilder;
use serde_json::json;
use std::io::Write;
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

use crate::internal::{EncodedProfile, Profile};
use crate::profiles::{Compressor, DefaultProfileCodec};

pub struct ProfileExporter {
    client: reqwest::Client,
    family: String,
    base_tags_string: String,
    request_url: String,
    headers: reqwest::header::HeaderMap,
    runtime: Option<Runtime>,
}

pub struct File<'a> {
    pub name: &'a str,
    pub bytes: &'a [u8],
}

impl ProfileExporter {
    /// Creates a new exporter to be used to report profiling data.
    ///
    /// Note: Reqwest v0.12.23+ includes automatic retry support for transient failures.
    /// The default configuration automatically retries safe errors and low-level protocol NACKs.
    /// For custom retry policies, users can configure the reqwest client before creating the
    /// exporter.
    ///
    /// # Thread Safety
    ///
    /// The exporter can be used from any thread, but if using `send_blocking()`, the exporter
    /// should remain on the same thread for all blocking calls. See [`send_blocking`] for details.
    ///
    /// [`send_blocking`]: ProfileExporter::send_blocking
    pub fn new(
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        mut tags: Vec<Tag>,
        endpoint: Endpoint,
    ) -> anyhow::Result<Self> {
        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(endpoint.timeout_ms));

        let request_url = match endpoint.url.scheme_str() {
            // HTTP/HTTPS endpoints
            Some("http") | Some("https") => endpoint.url.to_string(),

            // File dump endpoint (debugging) - uses platform-specific local transport
            Some("file") => {
                let output_path = libdd_common::decode_uri_path_in_authority(&endpoint.url)
                    .context("Failed to decode file path from URI")?;
                let socket_or_pipe_path = spawn_dump_server(output_path)?;

                // Configure the client to use the local socket/pipe
                #[cfg(unix)]
                {
                    builder = builder.unix_socket(socket_or_pipe_path);
                }
                #[cfg(windows)]
                {
                    builder = builder
                        .windows_named_pipe(socket_or_pipe_path.to_string_lossy().to_string());
                }

                "http://localhost/".to_string()
            }

            // Unix domain sockets
            #[cfg(unix)]
            Some("unix") => {
                use libdd_common::connector::uds::socket_path_from_uri;
                let socket_path = socket_path_from_uri(&endpoint.url)?;
                builder = builder.unix_socket(socket_path);
                format!("http://localhost{}", endpoint.url.path())
            }

            // Windows named pipes
            #[cfg(windows)]
            Some("windows") => {
                use libdd_common::connector::named_pipe::named_pipe_path_from_uri;
                let pipe_path = named_pipe_path_from_uri(&endpoint.url)?;
                builder = builder.windows_named_pipe(pipe_path.to_string_lossy().to_string());
                format!("http://localhost{}", endpoint.url.path())
            }

            // Unsupported schemes
            scheme => anyhow::bail!("Unsupported endpoint scheme: {:?}", scheme),
        };

        // Pre-build all static headers
        let mut headers = reqwest::header::HeaderMap::new();

        // Always-present headers
        headers.insert(
            "Connection",
            reqwest::header::HeaderValue::from_static("close"),
        );
        headers.insert(
            "DD-EVP-ORIGIN",
            reqwest::header::HeaderValue::from_str(profiling_library_name)?,
        );
        headers.insert(
            "DD-EVP-ORIGIN-VERSION",
            reqwest::header::HeaderValue::from_str(profiling_library_version)?,
        );
        headers.insert(
            "User-Agent",
            reqwest::header::HeaderValue::from_str(&format!(
                "DDProf/{}",
                env!("CARGO_PKG_VERSION")
            ))?,
        );

        // Optional headers (API key, test token)
        if let Some(api_key) = &endpoint.api_key {
            headers.insert(
                "DD-API-KEY",
                reqwest::header::HeaderValue::from_str(api_key)?,
            );
        }
        if let Some(test_token) = &endpoint.test_token {
            headers.insert(
                "X-Datadog-Test-Session-Token",
                reqwest::header::HeaderValue::from_str(test_token)?,
            );
        }

        // Add Azure App Services tags if available
        if let Some(aas) = &*azure_app_services::AAS_METADATA {
            let aas_tags = [
                ("aas.resource.id", aas.get_resource_id()),
                (
                    "aas.environment.extension_version",
                    aas.get_extension_version(),
                ),
                ("aas.environment.instance_id", aas.get_instance_id()),
                ("aas.environment.instance_name", aas.get_instance_name()),
                ("aas.environment.os", aas.get_operating_system()),
                ("aas.resource.group", aas.get_resource_group()),
                ("aas.site.name", aas.get_site_name()),
                ("aas.site.kind", aas.get_site_kind()),
                ("aas.site.type", aas.get_site_type()),
                ("aas.subscription.id", aas.get_subscription_id()),
            ];

            // Avoid infallible allocation paths when adding the Azure tags.
            // This is an upper bound since Tag::new can fail and we'll skip invalid tags.
            tags.try_reserve(aas_tags.len())?;

            tags.extend(
                aas_tags
                    .into_iter()
                    .filter_map(|(name, value)| Tag::new(name, value).ok()),
            );
        }

        // Precompute the base tags string (includes configured tags + Azure App Services tags)
        let base_tags_string: String = tags.iter().flat_map(|tag| [tag.as_ref(), ","]).collect();

        Ok(Self {
            client: builder.build()?,
            family: family.to_string(),
            base_tags_string,
            request_url,
            headers,
            runtime: None,
        })
    }

    /// Synchronously sends a profile to the configured endpoint.
    ///
    /// This is a blocking wrapper around the async [`send`] method. It lazily creates and caches
    /// a single-threaded tokio runtime on first use.
    ///
    /// # Thread Affinity
    ///
    /// **Important**: The cached runtime uses `new_current_thread()`, which has thread affinity.
    /// For best results, all calls to `send_blocking()` on the same exporter instance should be
    /// made from the same thread. Moving the exporter across threads between blocking calls may
    /// cause issues.
    ///
    /// If you need to use the exporter from multiple threads, consider either:
    /// - Creating a separate exporter instance per thread
    /// - Using the async [`send`] method directly from within a tokio runtime
    ///
    /// [`send`]: ProfileExporter::send
    #[allow(clippy::too_many_arguments)]
    pub fn send_blocking(
        &mut self,
        profile: EncodedProfile,
        additional_files: &[File<'_>],
        additional_tags: &[Tag],
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
        process_tags: Option<&str>,
        cancel: Option<&CancellationToken>,
    ) -> anyhow::Result<reqwest::StatusCode> {
        if self.runtime.is_none() {
            self.runtime = Some(
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?,
            );
        }

        Ok(self
            .runtime
            .as_ref()
            .context("Missing runtime")?
            .block_on(self.send(
                profile,
                additional_files,
                additional_tags,
                internal_metadata,
                info,
                process_tags,
                cancel,
            ))?
            .status())
    }

    pub(crate) fn build(
        &self,
        profile: EncodedProfile,
        additional_files: &[File<'_>],
        additional_tags: &[Tag],
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
        process_tags: Option<&str>,
    ) -> anyhow::Result<RequestBuilder> {
        let tags_profiler = self.build_tags_string(additional_tags)?;
        let event = self.build_event_json(
            &profile,
            additional_files,
            &tags_profiler,
            internal_metadata,
            info,
            process_tags,
        );

        let form = self.build_multipart_form(event, profile, additional_files)?;

        Ok(self
            .client
            .post(&self.request_url)
            .headers(self.headers.clone())
            .multipart(form))
    }

    #[allow(clippy::too_many_arguments)]
    /// Build and send a profile. Returns the HTTP status code.
    pub async fn send(
        &self,
        profile: EncodedProfile,
        additional_files: &[File<'_>],
        additional_tags: &[Tag],
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
        process_tags: Option<&str>,
        cancel: Option<&CancellationToken>,
    ) -> Result<reqwest::Response, SendError> {
        let request_builder = self.build(
            profile,
            additional_files,
            additional_tags,
            internal_metadata,
            info,
            process_tags,
        )?;

        // Send request with optional cancellation support
        if let Some(token) = cancel {
            token
                .run_until_cancelled(request_builder.send())
                .await
                .ok_or(SendError::Cancelled)?
                .map_err(SendError::RequestFailed)
        } else {
            request_builder
                .send()
                .await
                .map_err(SendError::RequestFailed)
        }
    }

    // Helper methods

    fn build_tags_string(&self, additional_tags: &[Tag]) -> anyhow::Result<String> {
        // Start with precomputed base tags (includes configured tags + Azure App Services tags)
        let mut tags = self.base_tags_string.clone();

        // Add additional tags with try_reserve to avoid OOM
        for tag in additional_tags {
            let t = tag.as_ref();
            tags.try_reserve(t.len() + ','.len_utf8())?;
            tags.push_str(t);
            tags.push(',');
        }

        // Add runtime platform tag (last, no trailing comma)
        {
            let t = tag!("runtime_platform", target_triple::TARGET);
            // Using try_reserve_exact since this is the last tag
            tags.try_reserve_exact(t.as_ref().len())?;
            tags.push_str(t.as_ref());
        }
        Ok(tags)
    }

    fn build_event_json(
        &self,
        profile: &EncodedProfile,
        additional_files: &[File],
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

    fn build_multipart_form(
        &self,
        event: serde_json::Value,
        profile: EncodedProfile,
        additional_files: &[File],
    ) -> anyhow::Result<reqwest::multipart::Form> {
        let event_bytes = serde_json::to_vec(&event)?;

        let mut form = reqwest::multipart::Form::new().part(
            "event",
            reqwest::multipart::Part::bytes(event_bytes)
                .file_name("event.json")
                .mime_str("application/json")?,
        );

        // Add additional files (compressed)
        for file in additional_files {
            let mut encoder = Compressor::<DefaultProfileCodec>::try_new(
                (file.bytes.len() >> 3).next_power_of_two(),
                10 * 1024 * 1024,
                Profile::COMPRESSION_LEVEL,
            )
            .context("failed to create compressor")?;
            encoder.write_all(file.bytes)?;

            form = form.part(
                file.name.to_string(),
                reqwest::multipart::Part::bytes(encoder.finish()?).file_name(file.name.to_string()),
            );
        }

        // Add profile
        Ok(form.part(
            "profile.pprof",
            reqwest::multipart::Part::bytes(profile.buffer).file_name("profile.pprof"),
        ))
    }
}
