// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use bytes::Bytes;
use datadog_trace_protobuf::pb;
use datadog_trace_utils::trace_utils::{self, SendData, TracerHeaderTags};
use ddcommon::{connector, Endpoint};
use hyper::{Body, Client, Method};
use log::error;
use std::{borrow::Borrow, collections::HashMap, str::FromStr};
use tokio::runtime::Runtime;

struct TracerTags {
    tracer_version: String,
    language: String,
    language_version: String,
    language_interpreter: String,
}

impl<'a> From<&'a TracerTags> for TracerHeaderTags<'a> {
    fn from(tags: &'a TracerTags) -> TracerHeaderTags<'a> {
        TracerHeaderTags::<'_> {
            lang: &tags.language,
            lang_version: &tags.language_version,
            tracer_version: &tags.tracer_version,
            lang_interpreter: &tags.language_interpreter,
            ..Default::default()
        }
    }
}

impl<'a> From<&'a TracerTags> for HashMap<&'static str, String> {
    fn from(tags: &'a TracerTags) -> HashMap<&'static str, String> {
        TracerHeaderTags::<'_> {
            lang: &tags.language,
            lang_version: &tags.language_version,
            tracer_version: &tags.tracer_version,
            lang_interpreter: &tags.language_interpreter,
            ..Default::default()
        }
        .into()
    }
}

pub struct TraceExporter {
    endpoint: Endpoint,
    tags: TracerTags,
    no_proxy: bool,
    runtime: Runtime,
}

impl TraceExporter {
    pub fn builder() -> TraceExporterBuilder {
        TraceExporterBuilder::default()
    }

    pub fn send(&self, data: &[u8], trace_count: usize) -> Result<String, String> {
        if self.no_proxy {
            self.send_deser_ser(data)
        } else {
            self.send_proxy(data, trace_count)
        }
    }

    fn send_proxy(&self, data: &[u8], trace_count: usize) -> Result<String, String> {
        let uri = self.endpoint.url.clone();
        self.runtime
            .block_on(async {
                let mut req_builder = hyper::Request::builder()
                    .uri(uri)
                    .header(
                        hyper::header::USER_AGENT,
                        concat!("Tracer/", env!("CARGO_PKG_VERSION")),
                    )
                    .method(Method::POST);

                let headers: HashMap<&'static str, String> = self.tags.borrow().into();

                for (key, value) in &headers {
                    req_builder = req_builder.header(*key, value);
                }
                req_builder = req_builder
                    .header(
                        hyper::header::CONTENT_TYPE,
                        ddcommon::header::APPLICATION_MSGPACK,
                    )
                    .header(
                        ddcommon::header::DATADOG_TRACE_COUNT,
                        trace_count.to_string().as_str(),
                    );
                let req = req_builder
                    .body(Body::from(Bytes::copy_from_slice(data)))
                    .unwrap();

                match Client::builder()
                    .build(connector::Connector::default())
                    .request(req)
                    .await
                {
                    Ok(response) => {
                        if response.status() != 200 {
                            let body_bytes = hyper::body::to_bytes(response.into_body()).await?;
                            let response_body =
                                String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
                            anyhow::bail!("Agent did not accept traces: {response_body}");
                        }
                        match hyper::body::to_bytes(response.into_body()).await {
                            Ok(body) => Ok(String::from_utf8_lossy(&body).to_string()),
                            Err(err) => {
                                anyhow::bail!("Error reading agent response body: {err}");
                            }
                        }
                    }
                    Err(err) => anyhow::bail!("Failed to send traces: {err}"),
                }
            })
            .or_else(|err| {
                error!("Error sending traces: {err}");
                Ok(String::from("{}"))
            })
    }

    fn send_deser_ser(&self, data: &[u8]) -> Result<String, String> {
        let size = data.len();
        let traces: Vec<Vec<pb::Span>> = match rmp_serde::from_slice(data) {
            Ok(res) => res,
            Err(err) => {
                error!("Error deserializing trace from request body: {err}");
                return Ok(String::from("{}"));
            }
        };

        if traces.is_empty() {
            error!("No traces deserialized from the request body.");
            return Ok(String::from("{}"));
        }

        let header_tags: TracerHeaderTags<'_> = (&self.tags).into();

        let tracer_payload =
            trace_utils::collect_trace_chunks(traces, &header_tags, |_chunk, _root_span_index| {});

        let send_data = SendData::new(size, tracer_payload, header_tags, &self.endpoint);
        self.runtime.block_on(async {
            match send_data.send().await {
                Ok(response) => match hyper::body::to_bytes(response.into_body()).await {
                    Ok(body) => Ok(String::from_utf8_lossy(&body).to_string()),
                    Err(err) => {
                        error!("Error reading agent response body: {err}");
                        Ok(String::from("{}"))
                    }
                },
                Err(err) => {
                    error!("Error sending traces: {err}");
                    Ok(String::from("{}"))
                }
            }
        })
    }
}

#[derive(Default)]
pub struct TraceExporterBuilder {
    host: Option<String>,
    port: Option<u16>,
    tracer_version: Option<String>,
    language: Option<String>,
    language_version: Option<String>,
    interpreter: Option<String>,
    no_proxy: bool,
}

impl TraceExporterBuilder {
    pub fn set_host(&mut self, host: &str) -> &mut TraceExporterBuilder {
        self.host = Some(String::from(host));
        self
    }

    pub fn set_port(&mut self, port: u16) -> &mut TraceExporterBuilder {
        self.port = Some(port);
        self
    }

    pub fn set_tracer_version(&mut self, tracer_version: &str) -> &mut TraceExporterBuilder {
        self.tracer_version = Some(String::from(tracer_version));
        self
    }

    pub fn set_language(&mut self, lang: &str) -> &mut TraceExporterBuilder {
        self.language = Some(String::from(lang));
        self
    }

    pub fn set_language_version(&mut self, lang_version: &str) -> &mut TraceExporterBuilder {
        self.language_version = Some(String::from(lang_version));
        self
    }

    pub fn set_language_interpreter(
        &mut self,
        lang_interpreter: &str,
    ) -> &mut TraceExporterBuilder {
        self.interpreter = Some(String::from(lang_interpreter));
        self
    }

    pub fn set_proxy(&mut self, proxy: bool) -> &mut TraceExporterBuilder {
        self.no_proxy = !proxy;
        self
    }

    pub fn build(&mut self) -> anyhow::Result<TraceExporter> {
        let version = if self.no_proxy { "v0.7" } else { "v0.4" };
        let endpoint = Endpoint {
            url: hyper::Uri::from_str(
                format!(
                    "http://{}:{}/{}/traces",
                    self.host.as_ref().unwrap_or(&"127.0.0.1".to_string()),
                    self.port.unwrap_or(8126),
                    version
                )
                .as_str(),
            )?,
            api_key: None,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(TraceExporter {
            endpoint,
            tags: TracerTags {
                tracer_version: self.tracer_version.clone().unwrap(),
                language_version: self.language_version.clone().unwrap(),
                language_interpreter: self.interpreter.clone().unwrap(),
                language: self.language.clone().unwrap(),
            },
            no_proxy: self.no_proxy,
            runtime,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new() {
        let mut builder = TraceExporterBuilder::default();
        let exporter = builder
            .set_host("192.168.1.1")
            .set_port(8127)
            .set_proxy(false)
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .build()
            .unwrap();

        assert_eq!(
            exporter.endpoint.url.to_string(),
            "http://192.168.1.1:8127/v0.7/traces"
        );
        assert_eq!(builder.host.unwrap(), "192.168.1.1");
        assert_eq!(builder.port.unwrap(), 8127);
        assert_eq!(builder.tracer_version.unwrap(), "v0.1");
        assert_eq!(builder.language.unwrap(), "nodejs");
        assert_eq!(builder.language_version.unwrap(), "1.0");
        assert_eq!(builder.interpreter.unwrap(), "v8");
    }

    #[test]
    fn new_defaults() {
        let mut builder = TraceExporterBuilder::default();
        let exporter = builder
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .build()
            .unwrap();

        assert_eq!(
            exporter.endpoint.url.to_string(),
            "http://127.0.0.1:8126/v0.4/traces"
        );
        assert_eq!(builder.tracer_version.unwrap(), "v0.1");
        assert_eq!(builder.language.unwrap(), "nodejs");
        assert_eq!(builder.language_version.unwrap(), "1.0");
        assert_eq!(builder.interpreter.unwrap(), "v8");
    }

    #[test]
    fn configure() {}
    #[test]
    fn export() {}
}
