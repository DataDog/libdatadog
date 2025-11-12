// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::debugger_defs::{DebuggerData, DebuggerPayload};
use constcat::concat;
use http_body_util::BodyExt;
use hyper::body::Bytes;
use hyper::http::uri::PathAndQuery;
use hyper::{Method, Uri};
use libdd_common::hyper_migration;
use libdd_common::tag::Tag;
use libdd_common::Endpoint;
use libdd_data_pipeline::agent_info::schema::AgentInfoStruct;
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
const AGENT_DEBUGGER_SNAPSHOTS_URL_PATH: &str = "/debugger/v2/input";
const AGENT_DEBUGGER_DIAGNOSTICS_URL_PATH: &str = "/debugger/v1/diagnostics";

#[derive(Clone, Default)]
pub struct Config {
    pub logs_endpoint: Option<Endpoint>,
    pub snapshots_endpoint: Option<Endpoint>,
    pub diagnostics_endpoint: Option<Endpoint>,
}

impl Config {
    pub fn set_endpoint(
        &mut self,
        mut logs_endpoint: Endpoint,
        mut diagnostics_endpoint: Endpoint,
    ) -> anyhow::Result<()> {
        let mut snapshots_endpoint = if diagnostics_endpoint.api_key.is_some() {
            diagnostics_endpoint.clone()
        } else {
            logs_endpoint.clone()
        };

        let mut logs_uri_parts = logs_endpoint.url.into_parts();
        let mut snapshots_uri_parts = snapshots_endpoint.url.into_parts();
        let mut diagnostics_uri_parts = diagnostics_endpoint.url.into_parts();

        #[allow(clippy::unwrap_used)]
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
            snapshots_uri_parts.path_and_query = Some(PathAndQuery::from_static(
                if snapshots_endpoint.api_key.is_some() {
                    DIRECT_DEBUGGER_DIAGNOSTICS_URL_PATH
                } else {
                    AGENT_DEBUGGER_SNAPSHOTS_URL_PATH
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
        snapshots_endpoint.url = Uri::from_parts(snapshots_uri_parts)?;
        diagnostics_endpoint.url = Uri::from_parts(diagnostics_uri_parts)?;
        self.logs_endpoint = Some(logs_endpoint);
        self.snapshots_endpoint = Some(snapshots_endpoint);
        self.diagnostics_endpoint = Some(diagnostics_endpoint);
        Ok(())
    }

    pub fn without_dedicated_snapshots_endpoint(&mut self) {
        self.snapshots_endpoint = self.logs_endpoint.clone();
    }
}

pub fn agent_info_supports_dedicated_snapshots_endpoint(info: &AgentInfoStruct) -> bool {
    info.endpoints
        .as_ref()
        .map(|endpoints| {
            endpoints
                .iter()
                .any(|endpoint| endpoint == AGENT_DEBUGGER_SNAPSHOTS_URL_PATH)
        })
        .unwrap_or(false)
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
pub enum DebuggerType {
    Diagnostics,
    Snapshots,
    Logs,
}

impl DebuggerType {
    pub fn of_payload(payload: &DebuggerPayload) -> DebuggerType {
        match payload.debugger {
            DebuggerData::Snapshot(ref snapshot) => {
                if snapshot.captures.is_some() {
                    DebuggerType::Snapshots
                } else {
                    DebuggerType::Logs
                }
            }
            DebuggerData::Diagnostics(_) => DebuggerType::Diagnostics,
        }
    }
}

pub fn encode<S: Eq + Hash + Serialize>(data: Vec<DebuggerPayload>) -> Vec<u8> {
    #[allow(clippy::unwrap_used)]
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

#[derive(Default)]
enum SenderFuture {
    #[default]
    Error,
    Outstanding(hyper_migration::ResponseFuture),
    Submitted(JoinHandle<anyhow::Result<hyper_migration::HttpResponse>>),
}

pub struct PayloadSender {
    future: SenderFuture,
    sender: hyper_migration::Sender,
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
        #[allow(clippy::unwrap_used)]
        let endpoint = match debugger_type {
            DebuggerType::Diagnostics => &config.diagnostics_endpoint,
            DebuggerType::Snapshots => &config.snapshots_endpoint,
            DebuggerType::Logs => &config.logs_endpoint,
        }
        .as_ref()
        .unwrap();

        let mut url = endpoint.url.clone();
        let mut parts = url.into_parts();

        #[allow(clippy::unwrap_used)]
        let query = format!(
            "{}?ddtags={}",
            parts.path_and_query.unwrap(),
            percent_encoded_tags
        );
        parts.path_and_query = Some(PathAndQuery::from_str(&query)?);
        url = Uri::from_parts(parts)?;

        let mut req = endpoint
            .to_request_builder(concat!("Tracer/", env!("CARGO_PKG_VERSION")))?
            .method(Method::POST)
            .uri(url);

        if endpoint.api_key.is_some() {
            req = req.header("DD-EVP-ORIGIN", "agent-debugger");
        }

        let (sender, body) = hyper_migration::Body::channel();

        let needs_boundary = debugger_type == DebuggerType::Diagnostics;
        let req = req.header(
            "Content-type",
            if needs_boundary {
                concat!("multipart/form-data; boundary=", BOUNDARY)
            } else {
                "application/json"
            },
        );

        let future = hyper_migration::new_default_client().request(req.body(body)?);
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

                self.future = SenderFuture::Submitted(tokio::spawn(async {
                    let resp = hyper_migration::into_response(future.await?);
                    Ok(resp)
                }));
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

    pub async fn finish(self) -> anyhow::Result<u32> {
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
                        let body_bytes = response.into_body().collect().await?.to_bytes();
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
