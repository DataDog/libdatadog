// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::borrow::Cow;
use std::future;
use std::io::Cursor;

use bytes::Bytes;
pub use chrono::{DateTime, Utc};
pub use ddcommon::tag::Tag;
pub use hyper::Uri;
use hyper_multipart_rfc7578::client::multipart;
use mime;
use serde_json::json;
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

use ddcommon::{connector, HttpClient, HttpResponse};
pub mod config;
mod errors;
pub use ddcommon::Endpoint;

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
    tags: Option<Vec<Tag>>,
}

pub struct File<'a> {
    pub name: &'a str,
    pub bytes: &'a [u8],
}

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
    pub fn new<IntoCow: Into<Cow<'static, str>>>(
        family: IntoCow,
        tags: Option<Vec<Tag>>,
        endpoint: Endpoint,
    ) -> anyhow::Result<ProfileExporter> {
        Ok(Self {
            exporter: Exporter::new()?,
            endpoint,
            family: family.into(),
            tags,
        })
    }

    /// Build a Request object representing the profile information provided.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        files: &[File],
        additional_tags: Option<&Vec<Tag>>,
        timeout: std::time::Duration,
        profile_library_name: &str,
        profile_library_version: &str,
    ) -> anyhow::Result<Request> {
        let mut form = multipart::Form::default();

        let mut tags_profiler = String::new();

        for tags in self.tags.as_ref().iter().chain(additional_tags.iter()) {
            for tag in tags.iter() {
                tags_profiler += tag.as_ref();
                tags_profiler += ",";
            }
        }

        tags_profiler.pop();

        let attachments: Vec<String> = files.iter().map(|file| file.name.to_owned()).collect();

        let event = json!({
            "attachments": attachments,
            "tags_profiler": tags_profiler,
            "start": start.format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string(),
            "end": end.format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string(),
            "family": self.family.as_ref(),
            "version":"4",
        })
        .to_string();

        form.add_reader_file_with_mime(
            "event",
            Cursor::new(event),
            "event.json",
            mime::APPLICATION_JSON,
        );

        for file in files {
            form.add_reader_file(
                format!("data[{}]", file.name),
                Cursor::new(file.bytes.to_owned()),
                file.name,
            )
        }

        let builder = self
            .endpoint
            .into_request_builder(concat!("DDProf/", env!("CARGO_PKG_VERSION")))?
            .method(http::Method::POST)
            .header("Connection", "close")
            .header("DD-EVP-ORIGIN", profile_library_name)
            .header("DD-EVP-ORIGIN-VERSION", profile_library_version);

        Ok(
            Request::from(form.set_body_convert::<hyper::Body, multipart::Body>(builder)?)
                .with_timeout(timeout),
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
}

impl Exporter {
    /// Creates a new Exporter, initializing the TLS stack.
    pub fn new() -> anyhow::Result<Self> {
        // Set idle to 0, which prevents the pipe being broken every 2nd request
        let client = hyper::Client::builder()
            .pool_max_idle_per_host(0)
            .build(connector::Connector::new());
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
