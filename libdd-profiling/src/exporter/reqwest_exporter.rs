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
//! use libdd_profiling::exporter::{config, reqwest_exporter::ProfileExporter};
//!
//! # fn main() -> anyhow::Result<()> {
//! let endpoint = config::file("/tmp/profile_dump.http")?;
//! let exporter = ProfileExporter::new("dd-trace-test", "1.0.0", "rust", vec![], endpoint)?;
//! // Each request will be saved to /tmp/profile_dump_TIMESTAMP.http
//! # Ok(())
//! # }
//! ```
//!
//! The dumped files contain the complete HTTP request including headers and body,
//! which can be useful for debugging or replaying requests.

use anyhow::Context;
use libdd_common::tag::Tag;
use libdd_common::{azure_app_services, tag, Endpoint};
use serde_json::json;
use std::{future, io::Write};
use tokio_util::sync::CancellationToken;

use crate::internal::{EncodedProfile, Profile};
use crate::profiles::{Compressor, DefaultProfileCodec};

#[cfg(unix)]
use std::path::PathBuf;

pub struct ProfileExporter {
    client: reqwest::Client,
    family: String,
    base_tags_string: String,
    request_url: String,
    headers: reqwest::header::HeaderMap,
}

pub struct File<'a> {
    pub name: &'a str,
    pub bytes: &'a [u8],
}

impl<'a> From<super::File<'a>> for File<'a> {
    fn from(file: super::File<'a>) -> Self {
        Self {
            name: file.name,
            bytes: file.bytes,
        }
    }
}

impl ProfileExporter {
    /// Creates a new exporter to be used to report profiling data.
    ///
    /// Note: Reqwest v0.12.23+ includes automatic retry support for transient failures.
    /// The default configuration automatically retries safe errors and low-level protocol NACKs.
    /// For custom retry policies, users can configure the reqwest client before creating the
    /// exporter.
    pub fn new(
        profiling_library_name: &str,
        profiling_library_version: &str,
        family: &str,
        mut tags: Vec<Tag>,
        endpoint: Endpoint,
    ) -> anyhow::Result<Self> {
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(std::time::Duration::from_millis(endpoint.timeout_ms));

        // Check if this is a file dump endpoint (for debugging)
        #[cfg(unix)]
        let request_url = if endpoint.url.scheme_str() == Some("file") {
            // Extract the file path from the file:// URL
            // The path is hex-encoded in the authority section
            let output_path = libdd_common::decode_uri_path_in_authority(&endpoint.url)
                .context("Failed to decode file path from URI")?;
            let socket_path = Self::spawn_dump_server(output_path)?;
            builder = builder.unix_socket(socket_path);
            "http://localhost/v1/input".to_string()
        } else if endpoint.url.scheme_str() == Some("unix") {
            use libdd_common::connector::uds::socket_path_from_uri;
            let socket_path = socket_path_from_uri(&endpoint.url)?;
            builder = builder.unix_socket(socket_path);
            format!("http://localhost{}", endpoint.url.path())
        } else {
            endpoint.url.to_string()
        };

        #[cfg(not(unix))]
        let request_url = match endpoint.url.scheme_str() {
            Some("file") => {
                anyhow::bail!(
                    "file:// endpoints are only supported on Unix platforms (requires Unix domain sockets)"
                )
            }
            Some("unix") => {
                anyhow::bail!("unix:// endpoints are only supported on Unix platforms")
            }
            _ => endpoint.url.to_string(),
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
            tags.extend(
                [
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
                ]
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
        })
    }

    /// Build and send a profile. Returns the HTTP status code.
    pub async fn send(
        &self,
        profile: EncodedProfile,
        additional_files: &[File<'_>],
        additional_tags: &[Tag],
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
        cancel: Option<&CancellationToken>,
    ) -> anyhow::Result<reqwest::StatusCode> {
        let tags_profiler = self.build_tags_string(additional_tags);
        let event = self.build_event_json(
            &profile,
            additional_files,
            &tags_profiler,
            internal_metadata,
            info,
        );

        let form = self.build_multipart_form(event, profile, additional_files)?;

        // Build request
        let request = self
            .client
            .post(&self.request_url)
            .headers(self.headers.clone())
            .multipart(form)
            .build()?;

        // Send request with cancellation support
        tokio::select! {
            _ = async {
                match cancel {
                    Some(token) => token.cancelled().await,
                    None => future::pending().await,
                }
            } => Err(anyhow::anyhow!("Operation cancelled by user")),
            result = self.client.execute(request) => {
                Ok(result?.status())
            }
        }
    }

    // Helper methods

    fn build_tags_string(&self, additional_tags: &[Tag]) -> String {
        // Start with precomputed base tags (includes configured tags + Azure App Services tags)
        let mut tags = self.base_tags_string.clone();

        // Add additional tags
        for tag in additional_tags {
            tags.push_str(tag.as_ref());
            tags.push(',');
        }

        // Add runtime platform tag (last, no trailing comma)
        tags.push_str(tag!("runtime_platform", target_triple::TARGET).as_ref());
        tags
    }

    fn build_event_json(
        &self,
        profile: &EncodedProfile,
        additional_files: &[File],
        tags_profiler: &str,
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
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
            "endpoint_counts": if profile.endpoints_stats.is_empty() {
                None
            } else {
                Some(&profile.endpoints_stats)
            },
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

    /// Spawns a HTTP dump server that saves incoming requests to a file
    /// Returns the Unix socket path that the server is listening on
    #[cfg(unix)]
    fn spawn_dump_server(output_path: PathBuf) -> anyhow::Result<PathBuf> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixListener;

        // Create a temporary socket path
        let socket_path =
            std::env::temp_dir().join(format!("libdatadog_dump_{}.sock", std::process::id()));

        // Remove socket file if it already exists
        let _ = std::fs::remove_file(&socket_path);

        let listener = UnixListener::bind(&socket_path)
            .context("Failed to bind Unix socket for dump server")?;

        let socket_path_clone = socket_path.clone();

        // Spawn the server task
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((mut stream, _)) => {
                        let output_path = output_path.clone();

                        tokio::spawn(async move {
                            // Read the HTTP request in chunks
                            let mut request_data = Vec::new();
                            let mut buffer = [0u8; 8192];
                            let mut content_length: Option<usize> = None;
                            let mut headers_end_pos: Option<usize> = None;

                            // Read headers first
                            loop {
                                match stream.read(&mut buffer).await {
                                    Ok(0) => break, // Connection closed
                                    Ok(n) => {
                                        request_data.extend_from_slice(&buffer[..n]);

                                        // Look for end of headers if we haven't found it yet
                                        if headers_end_pos.is_none() {
                                            if let Some(pos) =
                                                find_subsequence(&request_data, b"\r\n\r\n")
                                            {
                                                headers_end_pos = Some(pos + 4);

                                                // Parse Content-Length from headers
                                                if let Ok(headers_str) =
                                                    std::str::from_utf8(&request_data[..pos])
                                                {
                                                    for line in headers_str.lines() {
                                                        if line
                                                            .to_lowercase()
                                                            .starts_with("content-length:")
                                                        {
                                                            if let Some(len_str) =
                                                                line.split(':').nth(1)
                                                            {
                                                                content_length =
                                                                    len_str.trim().parse().ok();
                                                            }
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                        }

                                        // Check if we have the complete request
                                        if let Some(headers_end) = headers_end_pos {
                                            if let Some(expected_len) = content_length {
                                                let body_len = request_data.len() - headers_end;
                                                if body_len >= expected_len {
                                                    break; // Complete request received
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("Failed to read from dump server socket: {}", e);
                                        break;
                                    }
                                }
                            }

                            if !request_data.is_empty() {
                                // Generate filename with timestamp
                                let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f");
                                let final_path = if let Some(ext) = output_path.extension() {
                                    // Has extension, add timestamp before extension
                                    let stem = output_path
                                        .file_stem()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or("dump");
                                    let parent = output_path
                                        .parent()
                                        .unwrap_or_else(|| std::path::Path::new("."));
                                    parent.join(format!(
                                        "{}_{}.{}",
                                        stem,
                                        timestamp,
                                        ext.to_string_lossy()
                                    ))
                                } else {
                                    // No extension, append timestamp
                                    let filename = output_path
                                        .file_name()
                                        .and_then(|s| s.to_str())
                                        .unwrap_or("dump");
                                    let parent = output_path
                                        .parent()
                                        .unwrap_or_else(|| std::path::Path::new("."));
                                    parent.join(format!("{}_{}", filename, timestamp))
                                };

                                // Write the request to file
                                if let Err(e) = std::fs::write(&final_path, &request_data) {
                                    eprintln!(
                                        "Failed to write request dump to {:?}: {}",
                                        final_path, e
                                    );
                                } else {
                                    println!("HTTP request dumped to: {:?}", final_path);
                                }
                            }

                            // Send a simple HTTP 200 response
                            let response = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
                            let _ = stream.write_all(response).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("Failed to accept connection on dump server: {}", e);
                        break;
                    }
                }
            }
        });

        Ok(socket_path_clone)
    }
}

/// Helper function to find a subsequence in a byte slice
#[cfg(unix)]
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
