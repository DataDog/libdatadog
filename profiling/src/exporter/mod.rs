// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod config;

mod errors;

use bytes::Bytes;
use hyper_multipart_rfc7578::client::multipart;
use lz4_flex::frame::FrameEncoder;
use serde_json::json;
use std::borrow::Cow;
use std::future;
use std::io::{Cursor, Write};
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

use crate::internal::ProfiledEndpointsStats;
use ddcommon::azure_app_services;
use ddcommon_net1::{connector, Endpoint, HttpClient, HttpResponse};

pub use chrono::{DateTime, Utc};
pub use ddcommon::tag::Tag;
pub use hyper::Uri;

#[cfg(unix)]
pub use connector::uds::{socket_path_from_uri, socket_path_to_uri};

#[cfg(windows)]
pub use connector::named_pipe::{named_pipe_path_from_uri, named_pipe_path_to_uri};

const DURATION_ZERO: std::time::Duration = std::time::Duration::from_millis(0);

pub struct Exporter {
    client: HttpClient,
    runtime: Runtime,
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
    req: hyper::Request<hyper::Body>,
}

impl From<hyper::Request<hyper::Body>> for Request {
    fn from(req: hyper::Request<hyper::Body>) -> Self {
        Self { req, timeout: None }
    }
}

impl Request {
    fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = if timeout != DURATION_ZERO {
            Some(timeout)
        } else {
            None
        };
        self
    }

    pub fn timeout(&self) -> &Option<std::time::Duration> {
        &self.timeout
    }

    pub fn uri(&self) -> &hyper::Uri {
        self.req.uri()
    }

    pub fn headers(&self) -> &hyper::HeaderMap {
        self.req.headers()
    }

    pub fn body(self) -> hyper::Body {
        self.req.into_body()
    }

    async fn send(
        self,
        client: &HttpClient,
        cancel: Option<&CancellationToken>,
    ) -> anyhow::Result<hyper::Response<hyper::Body>> {
        tokio::select! {
            _ = async { match cancel {
                    Some(cancellation_token) => cancellation_token.cancelled().await,
                    // If no token is provided, future::pending() provides a no-op future that never resolves
                    None => future::pending().await,
                }}
            => Err(crate::exporter::errors::Error::UserRequestedCancellation.into()),
            result = async {
                Ok(match self.timeout {
                    Some(t) => tokio::time::timeout(t, client.request(self.req))
                        .await
                        .map_err(|_| crate::exporter::errors::Error::OperationTimedOut)?,
                    None => client.request(self.req).await,
                }?)}
            => result,
        }
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
            exporter: Exporter::new()?,
            endpoint,
            family: family.into(),
            profiling_library_name: profiling_library_name.into(),
            profiling_library_version: profiling_library_version.into(),
            tags,
        })
    }

    #[allow(clippy::too_many_arguments)]
    /// Build a Request object representing the profile information provided.
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
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        files_to_compress_and_export: &[File],
        files_to_export_unmodified: &[File],
        additional_tags: Option<&Vec<Tag>>,
        endpoint_counts: Option<&ProfiledEndpointsStats>,
        internal_metadata: Option<serde_json::Value>,
        info: Option<serde_json::Value>,
    ) -> anyhow::Result<Request> {
        let mut form = multipart::Form::default();

        // combine tags and additional_tags
        let mut tags_profiler = String::new();
        let other_tags = additional_tags.into_iter();
        for tag in self.tags.iter().chain(other_tags).flatten() {
            tags_profiler.push_str(tag.as_ref());
            tags_profiler.push(',');
        }

        if let Some(aas_metadata) = azure_app_services::get_metadata() {
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

        tags_profiler.pop(); // clean up the trailing comma

        let attachments: Vec<String> = files_to_compress_and_export
            .iter()
            .chain(files_to_export_unmodified.iter())
            .map(|file| file.name.to_owned())
            .collect();

        let event = json!({
            "attachments": attachments,
            "tags_profiler": tags_profiler,
            "start": start.format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string(),
            "end": end.format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string(),
            "family": self.family.as_ref(),
            "version": "4",
            "endpoint_counts" : endpoint_counts,
            "internal": internal_metadata.unwrap_or_else(|| json!({})),
            "info": info.unwrap_or_else(|| json!({})),
        })
        .to_string();

        form.add_reader_file_with_mime(
            // Intake does not look for filename=event.json, it looks for name=event.
            "event",
            // this one shouldn't be compressed
            Cursor::new(event),
            "event.json",
            mime::APPLICATION_JSON,
        );

        for file in files_to_compress_and_export {
            // We tend to have good compression ratios for the pprof files,
            // especially with timeline enabled. Not all files compress this
            // well, but these are just initial Vec sizes, not a hard-bound.
            // Using 1/10 gives us a better start than starting at zero, while
            // not reserving too much for things that compress really well, and
            // power-of-two capacities are almost always the best performing.
            let capacity = (file.bytes.len() / 10).next_power_of_two();
            let buffer = Vec::with_capacity(capacity);
            let mut encoder = FrameEncoder::new(buffer);
            encoder.write_all(file.bytes)?;
            let encoded = encoder.finish()?;
            /* The Datadog RFC examples strip off the file extension, but the exact behavior
             * isn't specified. This does the simple thing of using the filename
             * without modification for the form name because intake does not care
             * about these name of the form field for these attachments.
             */
            form.add_reader_file(file.name, Cursor::new(encoded), file.name);
        }

        for file in files_to_export_unmodified {
            let encoded = file.bytes.to_vec();
            /* The Datadog RFC examples strip off the file extension, but the exact behavior
             * isn't specified. This does the simple thing of using the filename
             * without modification for the form name because intake does not care
             * about these name of the form field for these attachments.
             */
            form.add_reader_file(file.name, Cursor::new(encoded), file.name)
        }

        let builder = self
            .endpoint
            .into_request_builder(concat!("DDProf/", env!("CARGO_PKG_VERSION")))?
            .method(http::Method::POST)
            .header("Connection", "close")
            .header("DD-EVP-ORIGIN", self.profiling_library_name.as_ref())
            .header(
                "DD-EVP-ORIGIN-VERSION",
                self.profiling_library_version.as_ref(),
            );

        Ok(
            Request::from(form.set_body_convert::<hyper::Body, multipart::Body>(builder)?)
                .with_timeout(std::time::Duration::from_millis(self.endpoint.timeout_ms)),
        )
    }

    pub fn send(
        &self,
        request: Request,
        cancel: Option<&CancellationToken>,
    ) -> anyhow::Result<HttpResponse> {
        self.exporter
            .runtime
            .block_on(request.send(&self.exporter.client, cancel))
    }

    pub fn set_timeout(&mut self, timeout_ms: u64) {
        self.endpoint.timeout_ms = timeout_ms;
    }
}

impl Exporter {
    /// Creates a new Exporter, initializing the TLS stack.
    pub fn new() -> anyhow::Result<Self> {
        // Set idle to 0, which prevents the pipe being broken every 2nd request
        let client = hyper::Client::builder()
            .pool_max_idle_per_host(0)
            .build(connector::Connector::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(Self { client, runtime })
    }

    pub fn send(
        &self,
        http_method: http::Method,
        url: &str,
        mut headers: hyper::header::HeaderMap,
        body: &[u8],
        timeout: std::time::Duration,
    ) -> anyhow::Result<hyper::Response<hyper::Body>> {
        self.runtime.block_on(async {
            let mut request = hyper::Request::builder()
                .method(http_method)
                .uri(url)
                .body(hyper::Body::from(Bytes::copy_from_slice(body)))?;
            std::mem::swap(request.headers_mut(), &mut headers);

            let request: Request = request.into();
            request.with_timeout(timeout).send(&self.client, None).await
        })
    }
}
