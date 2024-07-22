// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::debugger_defs::{DebuggerData, DebuggerPayload};
use constcat::concat;
use ddcommon::connector::Connector;
use ddcommon::tag::Tag;
use ddcommon::Endpoint;
use hyper::body::{Bytes, Sender};
use hyper::client::ResponseFuture;
use hyper::http::uri::PathAndQuery;
use hyper::{Body, Client, Method, Response, Uri};
use percent_encoding::{percent_encode, CONTROLS};
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::hash::Hash;
use std::str::FromStr;
use tokio::task::JoinHandle;
use uuid::Uuid;

pub const PROD_LOGS_INTAKE_SUBDOMAIN: &str = "http-intake.logs";
pub const PROD_DIAGNOSTICS_INTAKE_SUBDOMAIN: &str = "debugger-intake";

const DIRECT_DEBUGGER_LOGS_URL_PATH: &str = "/api/v2/logs";
const DIRECT_DEBUGGER_DIAGNOSTICS_URL_PATH: &str = "/api/v2/debugger";
const AGENT_DEBUGGER_LOGS_URL_PATH: &str = "/debugger/v1/input";
const AGENT_DEBUGGER_DIAGNOSTICS_URL_PATH: &str = "/debugger/v1/diagnostics";

#[derive(Clone, Default)]
pub struct Config {
    pub logs_endpoint: Option<Endpoint>,
    pub diagnostics_endpoint: Option<Endpoint>,
}

impl Config {
    pub fn set_endpoint(
        &mut self,
        mut logs_endpoint: Endpoint,
        mut diagnostics_endpoint: Endpoint,
    ) -> anyhow::Result<()> {
        let mut logs_uri_parts = logs_endpoint.url.into_parts();
        let mut diagnostics_uri_parts = diagnostics_endpoint.url.into_parts();
        if logs_uri_parts.scheme.is_some()
            && logs_uri_parts.scheme.as_ref().unwrap().as_str() != "file"
        {
            logs_uri_parts.path_and_query = Some(PathAndQuery::from_static(
                if logs_endpoint.api_key.is_some() {
                    DIRECT_DEBUGGER_LOGS_URL_PATH
                } else {
                    AGENT_DEBUGGER_LOGS_URL_PATH
                },
            ));
            diagnostics_uri_parts.path_and_query = Some(PathAndQuery::from_static(
                if diagnostics_endpoint.api_key.is_some() {
                    DIRECT_DEBUGGER_DIAGNOSTICS_URL_PATH
                } else {
                    AGENT_DEBUGGER_DIAGNOSTICS_URL_PATH
                },
            ));
        }

        logs_endpoint.url = Uri::from_parts(logs_uri_parts)?;
        diagnostics_endpoint.url = Uri::from_parts(diagnostics_uri_parts)?;
        self.logs_endpoint = Some(logs_endpoint);
        self.diagnostics_endpoint = Some(diagnostics_endpoint);
        Ok(())
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
pub enum DebuggerType {
    Diagnostics,
    Logs,
}

impl DebuggerType {
    pub fn of_payload(payload: &DebuggerPayload) -> DebuggerType {
        match payload.debugger {
            DebuggerData::Snapshot(_) => DebuggerType::Logs,
            DebuggerData::Diagnostics(_) => DebuggerType::Diagnostics,
        }
    }
}

pub fn encode<S: Eq + Hash + Serialize>(data: Vec<DebuggerPayload>) -> Vec<u8> {
    serde_json::to_vec(&data).unwrap()
}

pub fn generate_tags(
    debugger_version: &dyn Display,
    env: &dyn Display,
    version: &dyn Display,
    runtime_id: &dyn Display,
    custom_tags: &mut dyn Iterator<Item = &Tag>,
) -> String {
    let mut tags = format!(
        "debugger_version:{debugger_version},env:{env},version:{version},runtime_id:{runtime_id}"
    );
    if let Ok(hostname) = sys_info::hostname() {
        tags.push_str(",host_name:");
        tags.push_str(hostname.as_str());
    }
    for tag in custom_tags {
        tags.push(',');
        tags.push_str(tag.as_ref());
    }
    percent_encode(tags.as_bytes(), CONTROLS).to_string()
}

#[derive(Debug, Default)]
enum SenderFuture {
    #[default]
    Error,
    Outstanding(ResponseFuture),
    Submitted(JoinHandle<hyper::Result<Response<Body>>>),
}

pub struct PayloadSender {
    future: SenderFuture,
    sender: Sender,
    needs_boundary: bool,
    payloads: u32,
}

const BOUNDARY: &str = "------------------------44617461646f67";
const BOUNDARY_LINE: &str = concat!("--", BOUNDARY, "\r\n");

impl PayloadSender {
    pub fn new(
        config: &Config,
        debugger_type: DebuggerType,
        percent_encoded_tags: &str,
    ) -> anyhow::Result<Self> {
        let endpoint = match debugger_type {
            DebuggerType::Diagnostics => &config.diagnostics_endpoint,
            DebuggerType::Logs => &config.logs_endpoint,
        }
        .as_ref()
        .unwrap();

        let mut url = endpoint.url.clone();
        let mut parts = url.into_parts();
        let query = format!(
            "{}?ddtags={}",
            parts.path_and_query.unwrap(),
            percent_encoded_tags
        );
        parts.path_and_query = Some(PathAndQuery::from_str(&query)?);
        url = Uri::from_parts(parts)?;

        let mut req = hyper::Request::builder()
            .header(
                hyper::header::USER_AGENT,
                concat!("Tracer/", env!("CARGO_PKG_VERSION")),
            )
            .method(Method::POST)
            .uri(url);

        if endpoint.api_key.is_some() {
            req = req.header("DD-EVP-ORIGIN", "agent-debugger");
        }

        let (sender, body) = Body::channel();

        let needs_boundary = debugger_type == DebuggerType::Diagnostics;
        let req = req.header(
            "Content-type",
            if needs_boundary {
                concat!("multipart/form-data; boundary=", BOUNDARY)
            } else {
                "application/json"
            },
        );

        let future = Client::builder()
            .build(Connector::default())
            .request(req.body(body)?);
        Ok(PayloadSender {
            future: SenderFuture::Outstanding(future),
            sender,
            needs_boundary,
            payloads: 0,
        })
    }

    pub async fn append(&mut self, data: &[u8]) -> anyhow::Result<()> {
        let first = match std::mem::take(&mut self.future) {
            SenderFuture::Outstanding(future) => {
                if self.needs_boundary {
                    let header = concat!(
                        BOUNDARY_LINE,
                        "Content-Disposition: form-data; name=\"event\"; filename=\"event.json\"\r\n",
                        "Content-Type: application/json\r\n",
                        "\r\n",
                    );
                    self.sender.send_data(header.into()).await?;
                }

                self.future = SenderFuture::Submitted(tokio::spawn(future));
                true
            }
            future => {
                self.future = future;
                false
            }
        };

        // Skip the [] of the Vec
        let data = &data[..data.len() - 1];
        let mut data = data.to_vec();
        if !first {
            data[0] = b',';
        }
        self.sender.send_data(Bytes::from(data)).await?;

        self.payloads += 1;
        Ok(())
    }

    pub async fn finish(mut self) -> anyhow::Result<u32> {
        if let SenderFuture::Submitted(future) = self.future {
            // insert a trailing ]
            if self.needs_boundary {
                self.sender
                    .send_data(concat!("]\r\n", BOUNDARY_LINE).into())
                    .await?;
            } else {
                self.sender.send_data(Bytes::from_static(b"]")).await?;
            }

            drop(self.sender);
            match future.await? {
                Ok(response) => {
                    let status = response.status().as_u16();
                    if status >= 400 {
                        let body_bytes = hyper::body::to_bytes(response.into_body()).await?;
                        let response_body =
                            String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
                        anyhow::bail!(
                            "Server did not accept debugger payload ({status}): {response_body}"
                        );
                    }
                    Ok(self.payloads)
                }
                Err(e) => anyhow::bail!("Failed to send traces: {e}"),
            }
        } else {
            Ok(0)
        }
    }
}

pub async fn send(
    payload: &[u8],
    config: &Config,
    debugger_type: DebuggerType,
    percent_encoded_tags: &str,
) -> anyhow::Result<()> {
    let mut batch = PayloadSender::new(config, debugger_type, percent_encoded_tags)?;
    batch.append(payload).await?;
    batch.finish().await?;
    Ok(())
}

pub fn generate_new_id() -> Uuid {
    Uuid::new_v4()
}
