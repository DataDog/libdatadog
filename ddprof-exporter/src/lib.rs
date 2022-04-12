// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::borrow::Cow;
use std::error::Error;
use std::io::Cursor;
use std::str::FromStr;

use bytes::Bytes;
pub use chrono::{DateTime, Utc};
use hyper::header::HeaderValue;
pub use hyper::Uri;
use hyper_multipart_rfc7578::client::multipart;
use tokio::runtime::Runtime;

mod connector;
mod container_id;
mod errors;
pub mod tag;

pub use tag::*;

#[cfg(unix)]
pub use connector::uds::socket_path_to_uri;

const DURATION_ZERO: std::time::Duration = std::time::Duration::from_millis(0);
const DATADOG_CONTAINER_ID_HEADER: &str = "Datadog-Container-ID";

type HttpClient = hyper::Client<connector::Connector, hyper::Body>;

pub struct Exporter {
    client: HttpClient,
    runtime: Runtime,
}

pub struct FieldsV3 {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

pub struct Endpoint {
    url: Uri,
    api_key: Option<Cow<'static, str>>,
}

pub struct ProfileExporterV3 {
    exporter: Exporter,
    endpoint: Endpoint,
    family: Cow<'static, str>,
    tags: Option<Vec<Tag>>,
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
    ) -> Result<hyper::Response<hyper::Body>, Box<dyn std::error::Error>> {
        Ok(match self.timeout {
            Some(t) => tokio::time::timeout(t, client.request(self.req))
                .await
                .map_err(|_| crate::errors::Error::OperationTimedOut)?,
            None => client.request(self.req).await,
        }?)
    }
}

pub struct File<'a> {
    pub name: &'a str,
    pub bytes: &'a [u8],
}

impl Endpoint {
    /// Creates an Endpoint for talking to the Datadog agent.
    ///
    /// # Arguments
    /// * `base_url` - has protocol, host, and port e.g. http://localhost:8126/
    pub fn agent(base_url: Uri) -> Result<Endpoint, Box<dyn Error>> {
        let mut parts = base_url.into_parts();
        let p_q = match parts.path_and_query {
            None => None,
            Some(pq) => {
                let path = pq.path();
                let path = path.strip_suffix('/').unwrap_or(path);
                Some(format!("{}/profiling/v1/input", path).parse()?)
            }
        };
        parts.path_and_query = p_q;
        let url = Uri::from_parts(parts)?;
        Ok(Endpoint { url, api_key: None })
    }

    /// Creates an Endpoint for talking to the Datadog agent though a unix socket.
    ///
    /// # Arguments
    /// * `socket_path` - file system path to the socket
    #[cfg(unix)]
    pub fn agent_uds(path: &std::path::Path) -> Result<Endpoint, Box<dyn Error>> {
        let base_url = socket_path_to_uri(path)?;
        Self::agent(base_url)
    }

    /// Creates an Endpoint for talking to Datadog intake without using the agent.
    /// This is an experimental feature.
    ///
    /// # Arguments
    /// * `site` - e.g. "datadoghq.com".
    /// * `api_key`
    pub fn agentless<AsStrRef: AsRef<str>, IntoCow: Into<Cow<'static, str>>>(
        site: AsStrRef,
        api_key: IntoCow,
    ) -> Result<Endpoint, Box<dyn Error>> {
        let intake_url: String = format!("https://intake.profile.{}/v1/input", site.as_ref());

        Ok(Endpoint {
            url: Uri::from_str(intake_url.as_str())?,
            api_key: Some(api_key.into()),
        })
    }
}

impl ProfileExporterV3 {
    pub fn new<IntoCow: Into<Cow<'static, str>>>(
        family: IntoCow,
        tags: Option<Vec<Tag>>,
        endpoint: Endpoint,
    ) -> Result<ProfileExporterV3, Box<dyn Error>> {
        Ok(Self {
            exporter: Exporter::new()?,
            endpoint,
            family: family.into(),
            tags,
        })
    }

    /// Build a Request object representing the profile information provided.
    pub fn build(
        &self,
        start: chrono::DateTime<chrono::Utc>,
        end: chrono::DateTime<chrono::Utc>,
        files: &[File],
        additional_tags: Option<&Vec<Tag>>,
        timeout: std::time::Duration,
    ) -> Result<Request, Box<dyn Error>> {
        let mut form = multipart::Form::default();

        form.add_text("version", "3");
        form.add_text("start", start.format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string());
        form.add_text("end", end.format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string());
        form.add_text("family", self.family.to_owned());

        for tags in self.tags.as_ref().iter().chain(additional_tags.iter()) {
            for tag in tags.iter() {
                form.add_text("tags[]", tag.to_string());
            }
        }

        for file in files {
            form.add_reader_file(
                format!("data[{}]", file.name),
                Cursor::new(file.bytes.to_owned()),
                file.name,
            )
        }

        let mut builder = hyper::Request::builder()
            .method(http::Method::POST)
            .uri(self.endpoint.url.clone())
            .header("User-Agent", concat!("DDProf/", env!("CARGO_PKG_VERSION")))
            .header("Connection", "close");

        if let Some(api_key) = &self.endpoint.api_key {
            builder = builder.header(
                "DD-API-KEY",
                HeaderValue::from_str(api_key).expect("Error setting api_key"),
            );
        }

        if let Some(container_id) = container_id::get_container_id() {
            builder = builder.header(DATADOG_CONTAINER_ID_HEADER, container_id);
        }

        Ok(
            Request::from(form.set_body_convert::<hyper::Body, multipart::Body>(builder)?)
                .with_timeout(timeout),
        )
    }

    pub fn send(&self, request: Request) -> Result<hyper::Response<hyper::Body>, Box<dyn Error>> {
        self.exporter
            .runtime
            .block_on(async { request.send(&self.exporter.client).await })
    }
}

impl Exporter {
    /// Creates a new Exporter, initializing the TLS stack.
    pub fn new() -> Result<Self, Box<dyn Error>> {
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
    ) -> Result<hyper::Response<hyper::Body>, Box<dyn std::error::Error>> {
        self.runtime.block_on(async {
            let mut request = hyper::Request::builder()
                .method(http_method)
                .uri(url)
                .body(hyper::Body::from(Bytes::copy_from_slice(body)))?;
            std::mem::swap(request.headers_mut(), &mut headers);

            let request: Request = request.into();
            request.with_timeout(timeout).send(&self.client).await
        })
    }
}
