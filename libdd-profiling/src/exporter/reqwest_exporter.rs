// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Reqwest-based profiling exporter
//!
//! This is a simplified async implementation using reqwest.

use anyhow::Context;
use libdd_common::tag::Tag;
use libdd_common::{azure_app_services, tag, Endpoint};
use serde_json::json;
use std::{future, io::Write};
use tokio_util::sync::CancellationToken;

use crate::internal::{EncodedProfile, Profile};
use crate::profiles::{Compressor, DefaultProfileCodec};

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
        tags: Vec<Tag>,
        endpoint: Endpoint,
    ) -> anyhow::Result<Self> {
        let mut builder = reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(std::time::Duration::from_millis(endpoint.timeout_ms));

        // Check if this is a Unix Domain Socket
        #[cfg(unix)]
        if endpoint.url.scheme_str() == Some("unix") {
            use libdd_common::connector::uds::socket_path_from_uri;
            let socket_path = socket_path_from_uri(&endpoint.url)?;
            builder = builder.unix_socket(socket_path);
        }

        // For Unix Domain Sockets, we need to use http://localhost as the URL
        // The socket path is configured on the client, so we convert the URL here
        let request_url = if endpoint.url.scheme_str() == Some("unix") {
            format!("http://localhost{}", endpoint.url.path())
        } else {
            endpoint.url.to_string()
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

        let user_agent = format!("DDProf/{}", env!("CARGO_PKG_VERSION"));
        headers.insert(
            "User-Agent",
            reqwest::header::HeaderValue::from_str(&user_agent)?,
        );

        // Optional headers (API key, test token)
        // These can fail if they contain invalid characters, but we treat that as non-fatal
        // since they're provided by the user's configuration
        if let Some(api_key) = &endpoint.api_key {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(api_key) {
                headers.insert("DD-API-KEY", value);
            }
        }
        if let Some(test_token) = &endpoint.test_token {
            if let Ok(value) = reqwest::header::HeaderValue::from_str(test_token) {
                headers.insert("X-Datadog-Test-Session-Token", value);
            }
        }

        // Precompute the base tags string (includes configured tags + Azure App Services tags)
        let mut base_tags_string = String::new();
        for tag in &tags {
            base_tags_string.push_str(tag.as_ref());
            base_tags_string.push(',');
        }

        // Add Azure App Services tags if available
        if let Some(aas) = &*azure_app_services::AAS_METADATA {
            for (name, value) in [
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
            ] {
                if let Ok(tag) = Tag::new(name, value) {
                    base_tags_string.push_str(tag.as_ref());
                    base_tags_string.push(',');
                }
            }
        }

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
}
