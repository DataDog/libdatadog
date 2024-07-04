// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::debugger_defs::{DebuggerData, DebuggerPayload};
use ddcommon::connector::Connector;
use ddcommon::tag::Tag;
use ddcommon::Endpoint;
use hyper::http::uri::PathAndQuery;
use hyper::{Body, Client, Method, Uri};
use percent_encoding::{percent_encode, CONTROLS};
use serde::{Deserialize, Serialize};
use std::fmt::Display;
use std::hash::Hash;
use std::str::FromStr;
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
    pub fn set_endpoint(&mut self, mut logs_endpoint: Endpoint, mut diagnostics_endpoint: Endpoint) -> anyhow::Result<()> {
        let mut logs_uri_parts = logs_endpoint.url.into_parts();
        let mut diagnostics_uri_parts = diagnostics_endpoint.url.into_parts();
        if logs_uri_parts.scheme.is_some() && logs_uri_parts.scheme.as_ref().unwrap().as_str() != "file" {
            logs_uri_parts.path_and_query =
                Some(PathAndQuery::from_static(if logs_endpoint.api_key.is_some() {
                    DIRECT_DEBUGGER_LOGS_URL_PATH
                } else {
                    AGENT_DEBUGGER_LOGS_URL_PATH
                }));
            diagnostics_uri_parts.path_and_query =
                Some(PathAndQuery::from_static(if diagnostics_endpoint.api_key.is_some() {
                    DIRECT_DEBUGGER_DIAGNOSTICS_URL_PATH
                } else {
                    AGENT_DEBUGGER_DIAGNOSTICS_URL_PATH
                }));
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

pub async fn send(
    payload: &[u8],
    config: &Config,
    debugger_type: DebuggerType,
    percent_encoded_tags: &str,
) -> anyhow::Result<()> {
    // SAFETY: we ensure the reference exists across the request
    let payload = unsafe { std::mem::transmute::<&[u8], &[u8]>(payload) };

    let endpoint = match debugger_type {
        DebuggerType::Diagnostics => &config.diagnostics_endpoint,
        DebuggerType::Logs => &config.logs_endpoint,
    }.as_ref().unwrap();

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

    let req = if debugger_type == DebuggerType::Diagnostics {
        const BOUNDARY: &str = "------------------------44617461646f67";
        fn event_json(payload: &[u8]) -> Vec<u8> {
            fn write_boundary(data: &mut Vec<u8>) {
                data.extend_from_slice(b"--");
                data.extend_from_slice(BOUNDARY.as_bytes());
                data.extend_from_slice(b"\r\n");
            }
            let mut data = Vec::new();

            write_boundary(&mut data);
            data.extend_from_slice(b"Content-Disposition: form-data; name=\"event\"; filename=\"event.json\"\r\n");
            data.extend_from_slice(b"Content-Type: application/json\r\n");
            data.extend_from_slice(b"\r\n");
            data.extend_from_slice(payload);
            data.extend_from_slice(b"\r\n");
            write_boundary(&mut data);

            data
        }
        req.header("Content-type", format!("multipart/form-data; boundary={}", BOUNDARY))
           .body(Body::from(event_json(payload)))
    } else {
        req.header("Content-type", "application/json")
           .body(Body::from(payload))
    }?;

    match Client::builder()
        .build(Connector::default())
        .request(req)
        .await
    {
        Ok(response) => {
            let status = response.status().as_u16();
            if status >= 400 {
                let body_bytes = hyper::body::to_bytes(response.into_body()).await?;
                let response_body = String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
                anyhow::bail!("Server did not accept debugger payload ({status}): {response_body}");
            }
            Ok(())
        }
        Err(e) => anyhow::bail!("Failed to send traces: {e}"),
    }
}

pub fn generate_new_id() -> Uuid {
    Uuid::new_v4()
}
