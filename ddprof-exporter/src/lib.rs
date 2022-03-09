// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::borrow::Cow;
use std::error::Error;

use bytes::Bytes;
use reqwest::header::HeaderValue;
use reqwest::Url;
use tokio::runtime::Runtime;

mod container_id;

const DURATION_ZERO: std::time::Duration = std::time::Duration::from_millis(0);
const DATADOG_CONTAINER_ID_HEADER: &str = "Datadog-Container-ID";

pub struct Exporter {
    client: reqwest::Client,
    runtime: Runtime,
}

/// All tags are optional. Here are the currently known tag names:
/// * service
/// * language
/// * env
/// * version
/// * host - the agent will overwrite this; only useful in practice if the
///          experimental agent-less mode is being used.
/// Other tags may be used, such as the ones specified by the user in the
/// `DD_TAGS` env.
pub struct Tag {
    pub name: Cow<'static, str>,
    pub value: Cow<'static, str>,
}

pub struct FieldsV3 {
    pub start: chrono::DateTime<chrono::Utc>,
    pub end: chrono::DateTime<chrono::Utc>,
}

pub struct Endpoint {
    url: Url,
    api_key: Option<String>,
}

pub struct ProfileExporterV3 {
    exporter: Exporter,
    endpoint: Endpoint,
    family: String,
    tags: Vec<Tag>,
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
    pub fn agent(base_url: Url) -> Result<Endpoint, Box<dyn Error>> {
        Ok(Endpoint {
            url: base_url.join("/profiling/v1/input")?,
            api_key: None,
        })
    }

    /// Creates an Endpoint for talking to Datadog intake without using the agent.
    /// This is an experimental feature.
    ///
    /// # Arguments
    /// * `site` - e.g. "datadoghq.com".
    /// * `api_key`
    pub fn agentless<S: AsRef<str>>(site: S, api_key: S) -> Result<Endpoint, Box<dyn Error>> {
        let intake_url = format!("https://intake.profile.{}/v1/input", site.as_ref());

        Ok(Endpoint {
            url: Url::parse(intake_url.as_str())?,
            api_key: Some(String::from(api_key.as_ref())),
        })
    }
}

impl ProfileExporterV3 {
    pub fn new<S: Into<String>>(
        family: S,
        tags: Vec<Tag>,
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
        timeout: std::time::Duration,
    ) -> reqwest::Result<reqwest::Request> {
        let mut builder = self
            .exporter
            .client
            .request(reqwest::Method::POST, self.endpoint.url.clone())
            .header("User-Agent", concat!("DDProf/", env!("CARGO_PKG_VERSION")))
            .header("Connection", "close");

        if timeout != DURATION_ZERO {
            builder = builder.timeout(timeout);
        }

        let mut form = reqwest::multipart::Form::new()
            .text("version", "3")
            .text("start", start.format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string())
            .text("end", end.format("%Y-%m-%dT%H:%M:%S%.9fZ").to_string())
            .text("family", String::from(&self.family));

        for tag in self.tags.iter() {
            form = form.text("tags[]", format!("{}:{}", tag.name, tag.value));
        }

        form = files
            .iter()
            .fold(form, |form, file| -> reqwest::multipart::Form {
                let filename = file.name.to_owned();
                let bytes = reqwest::multipart::Part::bytes(file.bytes.to_owned())
                    .file_name(filename.clone())
                    .mime_str("application/octet-stream")
                    .expect("mime to be valid");

                form.part(format!("data[{}]", filename), bytes)
            });

        if let Some(api_key) = &self.endpoint.api_key {
            builder = builder.header(
                "DD-API-KEY",
                HeaderValue::from_str(api_key.as_str()).expect("TODO"),
            );
        }

        if let Some(container_id) = container_id::get_container_id() {
            builder = builder.header(DATADOG_CONTAINER_ID_HEADER, container_id);
        }

        builder.multipart(form).build()
    }

    pub fn send(&self, request: reqwest::Request) -> reqwest::Result<reqwest::Response> {
        self.exporter
            .runtime
            .block_on(async { self.exporter.client.execute(request).await })
    }
}

impl Exporter {
    /// Creates a new Exporter, initializing the TLS stack.
    pub fn new() -> Result<Self, Box<dyn Error>> {
        // Set idle to 0, which prevents the pipe being broken every 2nd request
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .build()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(Self { client, runtime })
    }

    pub fn send(
        &self,
        http_method: http::Method,
        url: &str,
        headers: reqwest::header::HeaderMap,
        body: &[u8],
        timeout: std::time::Duration,
    ) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
        self.runtime.block_on(async {
            let mut builder = self
                .client
                .request(http_method, url)
                .headers(headers)
                .body(reqwest::Body::from(Bytes::copy_from_slice(body)));

            if timeout != DURATION_ZERO {
                builder = builder.timeout(timeout)
            }

            let response = builder.send().await?;
            Ok(response)
        })
    }
}
