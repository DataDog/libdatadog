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
use std::borrow::Cow;
use std::fmt::Display;
use std::hash::Hash;
use std::str::FromStr;
use tokio::task::JoinHandle;
use uuid::Uuid;

pub const PROD_DIAGNOSTICS_INTAKE_SUBDOMAIN: &str = "debugger-intake";

const DIRECT_DEBUGGER_DIAGNOSTICS_URL_PATH: &str = "/api/v2/debugger";
const AGENT_DEBUGGER_SNAPSHOTS_URL_PATH: &str = "/debugger/v2/input";
const AGENT_DEBUGGER_DIAGNOSTICS_URL_PATH: &str = "/debugger/v1/diagnostics";

// The symbol database (SymDB) intake shares the debugger-intake host. Direct
// (agentless) uploads target /api/v2/debugger, while the agent exposes the
// /symdb/v1/input proxy route.
const DIRECT_SYMDB_URL_PATH: &str = "/api/v2/debugger";
const AGENT_SYMDB_URL_PATH: &str = "/symdb/v1/input";

/// Creates an [`Endpoint`] for sending debugger and SymDB payloads directly to
/// the Datadog intake without going through the agent (agentless mode).
///
/// This mirrors the agent's debugger proxy, which forwards to
/// `https://debugger-intake.{site}`. The per-track path (diagnostics, snapshots
/// or SymDB) is applied later by [`Config::set_endpoint`] /
/// [`Config::set_symdb_endpoint`].
///
/// # Arguments
/// * `site` - e.g. "datadoghq.com".
/// * `api_key`
pub fn debugger_intake_endpoint(
    site: &str,
    api_key: impl Into<Cow<'static, str>>,
) -> anyhow::Result<Endpoint> {
    Ok(Endpoint {
        url: Uri::from_str(&format!(
            "https://{PROD_DIAGNOSTICS_INTAKE_SUBDOMAIN}.{site}"
        ))?,
        api_key: Some(api_key.into()),
        ..Default::default()
    })
}

/// Derives the per-track intake path for an endpoint, returning a clone with
/// the path applied. Endpoints with an API key are treated as agentless (direct
/// intake) and use `direct_path`; otherwise the agent proxy `agent_path` is
/// used. `file://` endpoints (used for tests) are left untouched.
fn derive_endpoint_path(
    endpoint: &Endpoint,
    direct_path: &'static str,
    agent_path: &'static str,
) -> anyhow::Result<Endpoint> {
    let mut endpoint = endpoint.clone();
    let has_api_key = endpoint.api_key.is_some();
    let mut parts = endpoint.url.into_parts();
    let is_remote = parts
        .scheme
        .as_ref()
        .map(|scheme| scheme.as_str() != "file")
        .unwrap_or(false);
    if is_remote {
        let path = if has_api_key { direct_path } else { agent_path };
        parts.path_and_query = Some(PathAndQuery::from_static(path));
    }
    endpoint.url = Uri::from_parts(parts)?;
    Ok(endpoint)
}

#[derive(Clone, Default)]
pub struct Config {
    pub logs_endpoint: Option<Endpoint>,
    pub snapshots_endpoint: Option<Endpoint>,
    pub diagnostics_endpoint: Option<Endpoint>,
    pub symdb_endpoint: Option<Endpoint>,
    /// Additional debugger-intake endpoints to dual-ship logs/snapshots/diagnostics
    /// payloads to, mirroring the agent's `debugger_*_additional_endpoints`. Each
    /// entry holds the per-track path already derived.
    pub additional_logs_endpoints: Vec<Endpoint>,
    pub additional_snapshots_endpoints: Vec<Endpoint>,
    pub additional_diagnostics_endpoints: Vec<Endpoint>,
    /// Additional SymDB intake endpoints, mirroring the agent's
    /// `symdb_additional_endpoints`.
    pub additional_symdb_endpoints: Vec<Endpoint>,
}

impl Config {
    pub fn set_endpoint(&mut self, diagnostics_endpoint: Endpoint) -> anyhow::Result<()> {
        self.logs_endpoint = Some(derive_endpoint_path(
            &diagnostics_endpoint,
            DIRECT_DEBUGGER_DIAGNOSTICS_URL_PATH,
            AGENT_DEBUGGER_SNAPSHOTS_URL_PATH,
        )?);
        self.snapshots_endpoint = Some(derive_endpoint_path(
            &diagnostics_endpoint,
            DIRECT_DEBUGGER_DIAGNOSTICS_URL_PATH,
            AGENT_DEBUGGER_SNAPSHOTS_URL_PATH,
        )?);
        self.diagnostics_endpoint = Some(derive_endpoint_path(
            &diagnostics_endpoint,
            DIRECT_DEBUGGER_DIAGNOSTICS_URL_PATH,
            AGENT_DEBUGGER_DIAGNOSTICS_URL_PATH,
        )?);
        Ok(())
    }

    /// Sets the SymDB (symbol database) intake endpoint, deriving the
    /// `/api/v2/debugger` (agentless) or `/symdb/v1/input` (agent) path.
    pub fn set_symdb_endpoint(&mut self, symdb_endpoint: Endpoint) -> anyhow::Result<()> {
        self.symdb_endpoint = Some(derive_endpoint_path(
            &symdb_endpoint,
            DIRECT_SYMDB_URL_PATH,
            AGENT_SYMDB_URL_PATH,
        )?);
        Ok(())
    }

    /// Adds an additional debugger-intake endpoint to dual-ship every
    /// logs/snapshots/diagnostics payload to, mirroring the agent's
    /// `debugger_*_additional_endpoints`.
    pub fn add_additional_debugger_endpoint(&mut self, endpoint: Endpoint) -> anyhow::Result<()> {
        self.additional_logs_endpoints.push(derive_endpoint_path(
            &endpoint,
            DIRECT_DEBUGGER_DIAGNOSTICS_URL_PATH,
            AGENT_DEBUGGER_SNAPSHOTS_URL_PATH,
        )?);
        self.additional_snapshots_endpoints
            .push(derive_endpoint_path(
                &endpoint,
                DIRECT_DEBUGGER_DIAGNOSTICS_URL_PATH,
                AGENT_DEBUGGER_SNAPSHOTS_URL_PATH,
            )?);
        self.additional_diagnostics_endpoints
            .push(derive_endpoint_path(
                &endpoint,
                DIRECT_DEBUGGER_DIAGNOSTICS_URL_PATH,
                AGENT_DEBUGGER_DIAGNOSTICS_URL_PATH,
            )?);
        Ok(())
    }

    /// Adds an additional SymDB intake endpoint, mirroring the agent's
    /// `symdb_additional_endpoints`.
    pub fn add_additional_symdb_endpoint(&mut self, endpoint: Endpoint) -> anyhow::Result<()> {
        self.additional_symdb_endpoints.push(derive_endpoint_path(
            &endpoint,
            DIRECT_SYMDB_URL_PATH,
            AGENT_SYMDB_URL_PATH,
        )?);
        Ok(())
    }

    pub fn downgrade_to_diagnostics_endpoint(&mut self) {
        self.snapshots_endpoint = self.diagnostics_endpoint.clone();
        self.logs_endpoint = self.diagnostics_endpoint.clone();
    }

    /// Returns the endpoints for a debugger track: the primary endpoint first,
    /// followed by any additional dual-ship endpoints.
    fn debugger_endpoints_for(&self, debugger_type: DebuggerType) -> Vec<&Endpoint> {
        let (primary, additional) = match debugger_type {
            DebuggerType::Diagnostics => (
                &self.diagnostics_endpoint,
                &self.additional_diagnostics_endpoints,
            ),
            DebuggerType::Snapshots => (
                &self.snapshots_endpoint,
                &self.additional_snapshots_endpoints,
            ),
            DebuggerType::Logs => (&self.logs_endpoint, &self.additional_logs_endpoints),
        };
        let mut endpoints = Vec::with_capacity(1 + additional.len());
        endpoints.extend(primary.as_ref());
        endpoints.extend(additional.iter());
        endpoints
    }

    /// Returns the SymDB endpoints: the primary endpoint first, followed by any
    /// additional dual-ship endpoints.
    fn symdb_endpoints(&self) -> Vec<&Endpoint> {
        let mut endpoints = Vec::with_capacity(1 + self.additional_symdb_endpoints.len());
        endpoints.extend(self.symdb_endpoint.as_ref());
        endpoints.extend(self.additional_symdb_endpoints.iter());
        endpoints
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
        let endpoint = match debugger_type {
            DebuggerType::Diagnostics => &config.diagnostics_endpoint,
            DebuggerType::Snapshots => &config.snapshots_endpoint,
            DebuggerType::Logs => &config.logs_endpoint,
        }
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing endpoint for {debugger_type:?}"))?;
        Self::new_to_endpoint(endpoint, debugger_type, percent_encoded_tags)
    }

    /// Creates a sender targeting a specific endpoint. Used to fan a payload out
    /// to the primary plus any additional dual-ship endpoints.
    pub fn new_to_endpoint(
        endpoint: &Endpoint,
        debugger_type: DebuggerType,
        percent_encoded_tags: &str,
    ) -> anyhow::Result<Self> {
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
    let endpoints = config.debugger_endpoints_for(debugger_type);
    let (primary, additional) = endpoints
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("no endpoint configured for {debugger_type:?}"))?;

    // Send the primary first so a slow or stalled additional endpoint cannot
    // delay the intake whose result the caller actually depends on.
    let result = send_to_endpoint(payload, primary, debugger_type, percent_encoded_tags).await;

    // Best-effort dual-shipping to any additional endpoints, mirroring the
    // agent's `*_additional_endpoints` fan-out where non-primary responses are
    // discarded. Only the primary endpoint's result is returned to the caller.
    for &endpoint in additional {
        let _ = send_to_endpoint(payload, endpoint, debugger_type, percent_encoded_tags).await;
    }
    result
}

async fn send_to_endpoint(
    payload: &[u8],
    endpoint: &Endpoint,
    debugger_type: DebuggerType,
    percent_encoded_tags: &str,
) -> anyhow::Result<()> {
    let mut batch = PayloadSender::new_to_endpoint(endpoint, debugger_type, percent_encoded_tags)?;
    batch.append(payload).await?;
    batch.finish().await?;
    Ok(())
}

/// Forwards a raw SymDB (symbol database) payload to the configured SymDB intake,
/// mirroring the agent's `/symdb/v1/input` proxy. Unlike debugger payloads, the
/// body is forwarded verbatim, the tags ride in the `X-Datadog-Additional-Tags`
/// header (not the `ddtags` query string), and the origin is `agent-symdb`.
pub async fn send_symdb(
    payload: &[u8],
    content_type: &str,
    config: &Config,
    tags: &str,
) -> anyhow::Result<()> {
    let endpoints = config.symdb_endpoints();
    let (primary, additional) = endpoints
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("no symdb endpoint configured"))?;

    // Send the primary first so a slow or stalled additional endpoint cannot
    // delay the intake whose result the caller actually depends on.
    let result = send_symdb_to_endpoint(payload, content_type, primary, tags).await;

    for &endpoint in additional {
        let _ = send_symdb_to_endpoint(payload, content_type, endpoint, tags).await;
    }
    result
}

async fn send_symdb_to_endpoint(
    payload: &[u8],
    content_type: &str,
    endpoint: &Endpoint,
    tags: &str,
) -> anyhow::Result<()> {
    let mut req = endpoint
        .to_request_builder(concat!("Tracer/", env!("CARGO_PKG_VERSION")))?
        .method(Method::POST)
        .header("X-Datadog-Additional-Tags", tags)
        .header("Content-type", content_type);

    // The EVP origin only matters on the direct intake (agentless). In agent
    // mode the agent sets it when proxying, so gate it on the API key to match
    // the debugger tracks.
    if endpoint.api_key.is_some() {
        req = req.header("DD-EVP-ORIGIN", "agent-symdb");
    }

    let body = http_common::Body::from(payload.to_vec());
    let response = http_common::into_response(
        http_common::new_default_client()
            .request(req.body(body)?)
            .await?,
    );

    let status = response.status().as_u16();
    if status >= 400 {
        let body_bytes = response.into_body().collect().await?.to_bytes();
        let response_body = String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
        anyhow::bail!("Server did not accept symdb payload ({status}): {response_body}");
    }
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
    fn test_debugger_intake_endpoint() {
        let endpoint = debugger_intake_endpoint("datadoghq.com", "test-api-key").unwrap();
        assert_eq!(endpoint.url.host(), Some("debugger-intake.datadoghq.com"));
        assert_eq!(endpoint.url.scheme_str(), Some("https"));
        assert_eq!(endpoint.api_key.as_deref(), Some("test-api-key"));
    }

    #[test]
    fn test_set_symdb_endpoint_direct_mode() {
        let mut config = Config::default();
        config.set_symdb_endpoint(direct_endpoint()).unwrap();
        assert_eq!(endpoint_path(&config.symdb_endpoint), "/api/v2/debugger");
    }

    #[test]
    fn test_set_symdb_endpoint_agent_mode() {
        let mut config = Config::default();
        config.set_symdb_endpoint(agent_endpoint()).unwrap();
        assert_eq!(endpoint_path(&config.symdb_endpoint), "/symdb/v1/input");
    }

    #[test]
    fn test_additional_debugger_endpoints_derive_paths() {
        let mut config = Config::default();
        config.set_endpoint(direct_endpoint()).unwrap();
        config
            .add_additional_debugger_endpoint(direct_endpoint())
            .unwrap();

        let diagnostics = config.debugger_endpoints_for(DebuggerType::Diagnostics);
        assert_eq!(diagnostics.len(), 2);
        for endpoint in diagnostics {
            assert_eq!(endpoint.url.path(), "/api/v2/debugger");
        }
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
