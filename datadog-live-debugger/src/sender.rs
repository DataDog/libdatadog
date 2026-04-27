// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::debugger_defs::{DebuggerData, DebuggerPayload};
use bytes::Bytes;
use constcat::concat;
use http::uri::PathAndQuery;
use http::{Method, Uri};
use http_body_util::BodyExt;
use libdd_common::http_common;
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

pub const PROD_DIAGNOSTICS_INTAKE_SUBDOMAIN: &str = "debugger-intake";

const DIRECT_DEBUGGER_DIAGNOSTICS_URL_PATH: &str = "/api/v2/debugger";
const AGENT_DEBUGGER_SNAPSHOTS_URL_PATH: &str = "/debugger/v2/input";
const AGENT_DEBUGGER_DIAGNOSTICS_URL_PATH: &str = "/debugger/v1/diagnostics";

#[derive(Clone, Default)]
pub struct Config {
    pub logs_endpoint: Option<Endpoint>,
    pub snapshots_endpoint: Option<Endpoint>,
    pub diagnostics_endpoint: Option<Endpoint>,
}

impl Config {
    pub fn set_endpoint(&mut self, mut diagnostics_endpoint: Endpoint) -> anyhow::Result<()> {
        let mut logs_endpoint = diagnostics_endpoint.clone();
        let mut snapshots_endpoint = diagnostics_endpoint.clone();

        let mut logs_uri_parts = logs_endpoint.url.into_parts();
        let mut snapshots_uri_parts = snapshots_endpoint.url.into_parts();
        let mut diagnostics_uri_parts = diagnostics_endpoint.url.into_parts();

        #[allow(clippy::unwrap_used)]
        if diagnostics_uri_parts.scheme.is_some()
            && diagnostics_uri_parts.scheme.as_ref().unwrap().as_str() != "file"
        {
            let v2_path = PathAndQuery::from_static(if diagnostics_endpoint.api_key.is_some() {
                DIRECT_DEBUGGER_DIAGNOSTICS_URL_PATH
            } else {
                AGENT_DEBUGGER_SNAPSHOTS_URL_PATH
            });
            logs_uri_parts.path_and_query = Some(v2_path.clone());
            snapshots_uri_parts.path_and_query = Some(v2_path);
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

    pub fn downgrade_to_diagnostics_endpoint(&mut self) {
        self.snapshots_endpoint = self.diagnostics_endpoint.clone();
        self.logs_endpoint = self.diagnostics_endpoint.clone();
    }
}

pub fn agent_info_supports_debugger_v2_endpoint(info: &AgentInfoStruct) -> bool {
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
    Outstanding(http_common::ResponseFuture),
    Submitted(JoinHandle<anyhow::Result<http_common::HttpResponse>>),
}

pub struct PayloadSender {
    future: SenderFuture,
    sender: http_common::Sender,
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

        let (sender, body) = http_common::Body::channel();

        let needs_boundary = debugger_type == DebuggerType::Diagnostics;
        let req = req.header(
            "Content-type",
            if needs_boundary {
                concat!("multipart/form-data; boundary=", BOUNDARY)
            } else {
                "application/json"
            },
        );

        let future = http_common::new_default_client().request(req.body(body)?);
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
                    let resp = http_common::into_response(future.await?);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    fn agent_endpoint() -> Endpoint {
        Endpoint::from_slice("http://localhost:8126")
    }

    fn direct_endpoint() -> Endpoint {
        Endpoint {
            url: Uri::from_static("https://debugger-intake.datadoghq.com"),
            api_key: Some(Cow::Borrowed("test-api-key")),
            ..Default::default()
        }
    }

    fn endpoint_path(endpoint: &Option<Endpoint>) -> &str {
        endpoint
            .as_ref()
            .unwrap()
            .url
            .path_and_query()
            .unwrap()
            .as_str()
    }

    #[test]
    fn test_set_endpoint_agent_mode() {
        let mut config = Config::default();
        config.set_endpoint(agent_endpoint()).unwrap();

        assert_eq!(endpoint_path(&config.logs_endpoint), "/debugger/v2/input");
        assert_eq!(
            endpoint_path(&config.snapshots_endpoint),
            "/debugger/v2/input"
        );
        assert_eq!(
            endpoint_path(&config.diagnostics_endpoint),
            "/debugger/v1/diagnostics"
        );
    }

    #[test]
    fn test_set_endpoint_direct_mode() {
        let mut config = Config::default();
        config.set_endpoint(direct_endpoint()).unwrap();

        assert_eq!(endpoint_path(&config.logs_endpoint), "/api/v2/debugger");
        assert_eq!(
            endpoint_path(&config.snapshots_endpoint),
            "/api/v2/debugger"
        );
        assert_eq!(
            endpoint_path(&config.diagnostics_endpoint),
            "/api/v2/debugger"
        );
    }

    #[test]
    fn test_downgrade_to_diagnostics_endpoint() {
        let mut config = Config::default();
        config.set_endpoint(agent_endpoint()).unwrap();
        config.downgrade_to_diagnostics_endpoint();

        assert_eq!(
            endpoint_path(&config.logs_endpoint),
            "/debugger/v1/diagnostics"
        );
        assert_eq!(
            endpoint_path(&config.snapshots_endpoint),
            "/debugger/v1/diagnostics"
        );
        assert_eq!(
            endpoint_path(&config.diagnostics_endpoint),
            "/debugger/v1/diagnostics"
        );
    }

    #[test]
    fn test_agent_info_supports_debugger_v2_endpoint() {
        let with_v2 = AgentInfoStruct {
            endpoints: Some(vec!["/debugger/v2/input".to_string()]),
            ..Default::default()
        };
        assert!(agent_info_supports_debugger_v2_endpoint(&with_v2));

        let without_v2 = AgentInfoStruct {
            endpoints: Some(vec!["/debugger/v1/diagnostics".to_string()]),
            ..Default::default()
        };
        assert!(!agent_info_supports_debugger_v2_endpoint(&without_v2));

        let no_endpoints = AgentInfoStruct {
            endpoints: None,
            ..Default::default()
        };
        assert!(!agent_info_supports_debugger_v2_endpoint(&no_endpoints));
    }
}
