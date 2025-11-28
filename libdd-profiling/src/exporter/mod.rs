// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;
pub use chrono::{DateTime, Utc};
pub use http::Uri;
pub use libdd_common::tag::Tag;
use reqwest::blocking::multipart;
use serde_json::json;
use std::borrow::Cow;
use std::fmt::Debug;
use std::io::{Cursor, Write};
use std::iter;

use libdd_common::{azure_app_services, connector, tag, Endpoint};

pub mod config;

#[cfg(unix)]
pub use connector::uds::{socket_path_from_uri, socket_path_to_uri};

#[cfg(windows)]
pub use connector::named_pipe::{named_pipe_path_from_uri, named_pipe_path_to_uri};

use crate::internal::{EncodedProfile, Profile};
use crate::profiles::{Compressor, DefaultProfileCodec};

pub struct Exporter {
    client: reqwest::blocking::Client,
}

pub struct Fields {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

pub struct ProfileExporter {
    exporter: Exporter,
    endpoint: Endpoint,
    family: Cow<'static, str>,
    profiling_library_name: Cow<'static, str>,
    profiling_library_version: Cow<'static, str>,
    tags: Option<Vec<Tag>>,
}

pub struct File<'a> {
    pub name: &'a str,
    pub bytes: &'a [u8],
}

#[derive(Debug)]
pub struct Request {
    timeout: Option<std::time::Duration>,
    req: reqwest::blocking::RequestBuilder,
    uri: http::Uri,
    headers: http::HeaderMap,
    event_json: String,
}

impl Request {
    pub fn timeout(&self) -> &Option<std::time::Duration> {
        &self.timeout
    }

    pub fn uri(&self) -> &http::Uri {
        &self.uri
    }

    pub fn headers(&self) -> &http::HeaderMap {
        &self.headers
    }

    pub fn event_json(&self) -> &str {
        &self.event_json
    }

    fn send(self) -> anyhow::Result<http::Response<Vec<u8>>> {
        let mut req = self.req;
        if let Some(timeout) = self.timeout {
            req = req.timeout(timeout);
        }
        let response = req.send()?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.bytes()?.to_vec();

        let mut builder = http::Response::builder().status(status);
        for (key, value) in headers {
            if let Some(key) = key {
                builder = builder.header(key, value);
            }
        }
        Ok(builder.body(bytes)?)
    }
}

impl ProfileExporter {
    /// Creates a new exporter to be used to report profiling data.
    /// # Arguments
    /// * `profiling_library_name` - Profiling library name, usually dd-trace-something, e.g. "dd-trace-rb". See
    ///   https://datadoghq.atlassian.net/wiki/spaces/PROF/pages/1538884229/Client#Header-values (Datadog internal link)
    ///   for a list of common values.
    /// * `profiling_library_version` - Version used when publishing the profiling library to a
    ///   package manager
    /// * `family` - Profile family, e.g. "ruby"
    /// * `tags` - Tags to include with every profile reported by this exporter. It's also possible
    ///   to include profile-specific tags, see `additional_tags` on `build`.
    /// * `endpoint` - Configuration for reporting data
    pub fn new<F, N, V>(
        profiling_library_name: N,
        profiling_library_version: V,
        family: F,
        tags: Option<Vec<Tag>>,
        endpoint: Endpoint,
    ) -> anyhow::Result<ProfileExporter>
    where
        F: Into<Cow<'static, str>>,
        N: Into<Cow<'static, str>>,
        V: Into<Cow<'static, str>>,
    {
        Ok(Self {
            exporter: Exporter::new(&endpoint)?,
            endpoint,
            family: family.into(),
            profiling_library_name: profiling_library_name.into(),
            profiling_library_version: profiling_library_version.into(),
            tags,
        })
    }

    /// The target triple. This is a string like:
    ///  - aarch64-apple-darwin
    ///  - x86_64-unknown-linux-gnu
    ///
    /// The name is which is a misnomer, it traditionally had 3 pieces, but
    /// it's commonly 4+ fragments today.
    const TARGET_TRIPLE: &'static str = target_triple::TARGET;

    #[inline]
    fn runtime_platform_tag(&self) -> Tag {
        tag!("runtime_platform", ProfileExporter::TARGET_TRIPLE)
    }

    #[allow(clippy::too_many_arguments)]
    /// Build a Request object representing the profile information provided.
    ///
    /// Consumes the `EncodedProfile`, which is unavailable for use after.
    ///
    /// For details on the `internal_metadata` parameter, please reference the Datadog-internal
    /// "RFC: Attaching internal metadata to pprof profiles".
    /// If you use this parameter, please update the RFC with your use-case, so we can keep track of
    /// how this is getting used.
    ///
    /// For details on the `info` parameter, please reference the Datadog-internal
    /// "RFC: Pprof System Info Support".
    pub fn build(
        &self,
        profile: EncodedProfile,
        files_to_compress_and_export: &[File],
        files_to_export_unmodified: &[File],
        additional_tags: Option<&Vec<Tag>>,
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
    ) -> anyhow::Result<Request> {
        let mut form = multipart::Form::new();

        // combine tags and additional_tags
        let mut tags_profiler = String::new();
        let other_tags = additional_tags.into_iter();
        for tag in self.tags.iter().chain(other_tags).flatten() {
            tags_profiler.push_str(tag.as_ref());
            tags_profiler.push(',');
        }

        if let Some(aas_metadata) = &*azure_app_services::AAS_METADATA {
            let aas_tags = [
                ("aas.resource.id", aas_metadata.get_resource_id()),
                (
                    "aas.environment.extension_version",
                    aas_metadata.get_extension_version(),
                ),
                (
                    "aas.environment.instance_id",
                    aas_metadata.get_instance_id(),
                ),
                (
                    "aas.environment.instance_name",
                    aas_metadata.get_instance_name(),
                ),
                ("aas.environment.os", aas_metadata.get_operating_system()),
                ("aas.resource.group", aas_metadata.get_resource_group()),
                ("aas.site.name", aas_metadata.get_site_name()),
                ("aas.site.kind", aas_metadata.get_site_kind()),
                ("aas.site.type", aas_metadata.get_site_type()),
                ("aas.subscription.id", aas_metadata.get_subscription_id()),
            ];
            aas_tags.into_iter().for_each(|(name, value)| {
                if let Ok(tag) = Tag::new(name, value) {
                    tags_profiler.push_str(tag.as_ref());
                    tags_profiler.push(',');
                }
            });
        }

        // Since this is the last tag, we add it without a comma afterward. If
        // any tags get added after this one, you'll need to add the comma
        // between them.
        tags_profiler.push_str(self.runtime_platform_tag().as_ref());

        let attachments: Vec<String> = files_to_compress_and_export
            .iter()
            .chain(files_to_export_unmodified.iter())
            .map(|file| file.name.to_owned())
            .chain(iter::once("profile.pprof".to_string()))
            .collect();

        let endpoint_counts = if profile.endpoints_stats.is_empty() {
            None
        } else {
            Some(profile.endpoints_stats)
        };
        let mut internal: serde_json::value::Value = internal_metadata.unwrap_or_else(|| json!({}));
        internal["libdatadog_version"] = json!(env!("CARGO_PKG_VERSION"));

        let event = json!({
            "attachments": attachments,
            "tags_profiler": tags_profiler,
            "start": DateTime::<Utc>::from(profile.start).format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string(),
            "end": DateTime::<Utc>::from(profile.end).format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string(),
            "family": self.family.as_ref(),
            "version": "4",
            "endpoint_counts" : endpoint_counts,
            "internal": internal,
            "info": info.unwrap_or_else(|| json!({})),
        })
        .to_string();

        let event_cursor = Cursor::new(event.clone());

        form = form.part(
            "event",
            multipart::Part::reader(event_cursor)
                .file_name("event.json")
                .mime_str(mime::APPLICATION_JSON.as_ref())?,
        );

        for file in files_to_compress_and_export {
            // We don't know the file types and how well they compress. So for
            // a size hint, we look at roughly 1/8th of the file size.
            let capacity = (file.bytes.len() >> 3).next_power_of_two();
            // Most proxies/web server have a size limit per attachment,
            // 10 MiB should be plenty for everything we upload.
            let max_capacity = 10 * 1024 * 1024;
            // We haven't yet tested compression for attachments other than
            // profiles, which are compressed already before this point. We're
            // re-using the  same level here for now.
            let compression_level = Profile::COMPRESSION_LEVEL;
            let mut encoder = Compressor::<DefaultProfileCodec>::try_new(
                capacity,
                max_capacity,
                compression_level,
            )
            .context("failed to create compressor")?;
            encoder.write_all(file.bytes)?;
            let encoded = encoder.finish()?;

            form = form.part(
                file.name.to_owned(),
                multipart::Part::reader(Cursor::new(encoded)).file_name(file.name.to_owned()),
            );
        }

        for file in files_to_export_unmodified {
            let encoded = file.bytes.to_vec();
            form = form.part(
                file.name.to_owned(),
                multipart::Part::reader(Cursor::new(encoded)).file_name(file.name.to_owned()),
            );
        }
        // Add the actual pprof
        form = form.part(
            "profile.pprof",
            multipart::Part::reader(Cursor::new(profile.buffer)).file_name("profile.pprof"),
        );

        // Build the request using reqwest
        let user_agent = concat!("DDProf/", env!("CARGO_PKG_VERSION"));

        let url_string = if self.endpoint.url.scheme_str() == Some("unix") {
            // Replace unix://... with http://localhost...
            let path = self
                .endpoint
                .url
                .path_and_query()
                .map(|pq| pq.as_str())
                .unwrap_or("");
            format!("http://localhost{}", path)
        } else {
            self.endpoint.url.to_string()
        };

        // We need to parse it back to http::Uri to store it in Request, or just store String?
        // The accessor returns &http::Uri.
        let uri: http::Uri = url_string.parse()?;

        let mut builder = self
            .exporter
            .client
            .request(reqwest::Method::POST, url_string);

        let mut headers = http::HeaderMap::new();
        headers.insert(reqwest::header::USER_AGENT, user_agent.parse()?);

        // Replicate logic from Endpoint::to_request_builder but for reqwest
        builder = builder.header(reqwest::header::USER_AGENT, user_agent);
        if let Some(api_key) = &self.endpoint.api_key {
            builder = builder.header("dd-api-key", api_key.as_ref());
            headers.insert("dd-api-key", api_key.as_ref().parse()?);
        }
        if let Some(token) = &self.endpoint.test_token {
            builder = builder.header("x-datadog-test-session-token", token.as_ref());
            headers.insert("x-datadog-test-session-token", token.as_ref().parse()?);
        }
        if let Some(container_id) = libdd_common::entity_id::get_container_id() {
            builder = builder.header("datadog-container-id", container_id);
            headers.insert("datadog-container-id", container_id.parse()?);
        }
        if let Some(entity_id) = libdd_common::entity_id::get_entity_id() {
            builder = builder.header("datadog-entity-id", entity_id);
            headers.insert("datadog-entity-id", entity_id.parse()?);
        }
        if let Some(external_env) = *libdd_common::entity_id::DD_EXTERNAL_ENV {
            builder = builder.header("datadog-external-env", external_env);
            headers.insert("datadog-external-env", external_env.parse()?);
        }

        builder = builder
            .header("Connection", "close")
            .header("DD-EVP-ORIGIN", self.profiling_library_name.as_ref())
            .header(
                "DD-EVP-ORIGIN-VERSION",
                self.profiling_library_version.as_ref(),
            );

        headers.insert("Connection", "close".parse()?);
        headers.insert(
            "DD-EVP-ORIGIN",
            self.profiling_library_name.as_ref().parse()?,
        );
        headers.insert(
            "DD-EVP-ORIGIN-VERSION",
            self.profiling_library_version.as_ref().parse()?,
        );

        builder = builder.multipart(form);

        let event_json = event.clone();

        Ok(Request {
            timeout: Some(std::time::Duration::from_millis(self.endpoint.timeout_ms)),
            req: builder,
            uri,
            headers,
            event_json,
        })
    }

    pub fn send(&self, request: Request) -> anyhow::Result<http::Response<Vec<u8>>> {
        // We ignore cancellation in blocking client for now
        request.send()
    }

    pub fn set_timeout(&mut self, timeout_ms: u64) {
        self.endpoint.timeout_ms = timeout_ms;
    }
}

impl Exporter {
    /// Creates a new Exporter.
    pub fn new(endpoint: &Endpoint) -> anyhow::Result<Self> {
        let mut builder = reqwest::blocking::Client::builder();

        // Check for UDS or Named Pipe
        if let Some(scheme) = endpoint.url.scheme() {
            if scheme.as_str() == "unix" {
                #[cfg(unix)]
                {
                    let path = libdd_common::decode_uri_path_in_authority(&endpoint.url)?;
                    builder = builder.unix_socket(path);
                }
            }
        }

        Ok(Self {
            client: builder.build()?,
        })
    }
}
