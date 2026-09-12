// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared FFE EVP route selection and transport.
//!
//! Agent-backed Feature Flags keep the historical fixed EVP v2 route. Agentless
//! Feature Flags discover a compatible local receiver and use it when possible,
//! falling back to authenticated direct intake without coupling the decision to
//! the tracing transport mode.

use crate::service::evp_proxy;
use http::uri::PathAndQuery;
use http::Method;
use libdd_capabilities::{Bytes, HttpClientCapability, HttpError, SleepCapability};
use libdd_common::Endpoint;
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, warn};

pub(crate) use evp_proxy::EVENT_PLATFORM_INTAKE_SUBDOMAIN as EVP_SUBDOMAIN_VALUE;
pub(crate) use evp_proxy::SUBDOMAIN_HEADER as EVP_SUBDOMAIN_HEADER;

const USER_AGENT: &str = concat!("ddtrace-sidecar/", crate::sidecar_version!());
const EVP_PROXY_V4_PATH: &str = "/evp_proxy/v4";
const EVP_PROXY_V2_PATH: &str = "/evp_proxy/v2";
const INFO_PATH: &str = "/info";
const TRACE_ENDPOINT_PATHS: &[&str] = &["/v0.4/traces", "/v0.5/traces", "/v1.0/traces"];
const MAX_INFO_RESPONSE_BYTES: usize = 1 << 20;
const EVP_ORIGIN_HEADER: &str = "DD-EVP-ORIGIN";
const EVP_ORIGIN_VERSION_HEADER: &str = "DD-EVP-ORIGIN-VERSION";
const DEFAULT_UNAVAILABLE_RECOVERY_COOLDOWN: Duration = Duration::from_secs(30);
pub const MAX_FFE_EVP_PRODUCER_IDENTITY_LENGTH: usize = 256;
// The Agent contract guarantees that these local responses reject the request
// before processing it, so replaying the same batch through direct intake is
// safe. An upstream 403 does not provide that guarantee.
const AGENT_ROUTE_REJECTION_STATUSES: &[u16] = &[404, 405];
const WSAECONNREFUSED: i32 = 10061;

static NEXT_TRANSPORT_ID: AtomicU64 = AtomicU64::new(1);

/// Logical SDK identity attached to FFE EVP requests.
///
/// This is deliberately distinct from the sidecar process identity: intake
/// needs to know which tracer produced the events, not which helper sent them.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct FfeEvpProducerIdentity {
    origin: String,
    version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FfeEvpProducerIdentityError {
    EmptyOrigin,
    EmptyVersion,
    OriginTooLong { length: usize },
    VersionTooLong { length: usize },
    InvalidOrigin,
    InvalidVersion,
}

impl Display for FfeEvpProducerIdentityError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyOrigin => formatter.write_str("EVP producer origin must not be empty"),
            Self::EmptyVersion => formatter.write_str("EVP producer version must not be empty"),
            Self::OriginTooLong { length } => write!(
                formatter,
                "EVP producer origin is {length} bytes; maximum is {MAX_FFE_EVP_PRODUCER_IDENTITY_LENGTH}"
            ),
            Self::VersionTooLong { length } => write!(
                formatter,
                "EVP producer version is {length} bytes; maximum is {MAX_FFE_EVP_PRODUCER_IDENTITY_LENGTH}"
            ),
            Self::InvalidOrigin => {
                formatter.write_str("EVP producer origin is not a valid HTTP header value")
            }
            Self::InvalidVersion => {
                formatter.write_str("EVP producer version is not a valid HTTP header value")
            }
        }
    }
}

impl std::error::Error for FfeEvpProducerIdentityError {}

impl FfeEvpProducerIdentity {
    pub fn new(
        origin: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, FfeEvpProducerIdentityError> {
        let origin = origin.into();
        validate_identity_field(&origin, true)?;
        let version = version.into();
        validate_identity_field(&version, false)?;
        Ok(Self { origin, version })
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    fn legacy_sidecar() -> Self {
        Self {
            origin: "ddtrace-sidecar".to_owned(),
            version: crate::sidecar_version!().to_owned(),
        }
    }
}

fn validate_identity_field(
    value: &str,
    is_origin: bool,
) -> Result<(), FfeEvpProducerIdentityError> {
    if value.trim().is_empty() {
        return Err(if is_origin {
            FfeEvpProducerIdentityError::EmptyOrigin
        } else {
            FfeEvpProducerIdentityError::EmptyVersion
        });
    }
    let length = value.len();
    if length > MAX_FFE_EVP_PRODUCER_IDENTITY_LENGTH {
        return Err(if is_origin {
            FfeEvpProducerIdentityError::OriginTooLong { length }
        } else {
            FfeEvpProducerIdentityError::VersionTooLong { length }
        });
    }
    if value.trim() != value || http::HeaderValue::try_from(value).is_err() {
        return Err(if is_origin {
            FfeEvpProducerIdentityError::InvalidOrigin
        } else {
            FfeEvpProducerIdentityError::InvalidVersion
        });
    }
    Ok(())
}

impl<'de> Deserialize<'de> for FfeEvpProducerIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireIdentity {
            origin: String,
            version: String,
        }

        let identity = WireIdentity::deserialize(deserializer)?;
        Self::new(identity.origin, identity.version).map_err(serde::de::Error::custom)
    }
}

/// Feature Flags configuration source controlling whether direct EVP fallback
/// is allowed. This is deliberately independent from the tracing endpoint.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum FfeConfigurationSource {
    /// Remote Configuration / Agent-backed Feature Flags. EVP remains Agent-only.
    Agent,
    /// CDN-delivered Feature Flags. EVP may fall back to direct intake.
    Agentless,
}

/// Explicit per-session FFE EVP configuration crossing the sidecar IPC boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FfeEvpTransportConfig {
    pub source: FfeConfigurationSource,
    pub agent_endpoint: Endpoint,
    pub direct_endpoint: Option<Endpoint>,
}

impl FfeEvpTransportConfig {
    pub fn agent(agent_endpoint: Endpoint) -> Self {
        Self {
            source: FfeConfigurationSource::Agent,
            agent_endpoint,
            direct_endpoint: None,
        }
    }

    pub fn agentless(agent_endpoint: Endpoint, direct_endpoint: Option<Endpoint>) -> Self {
        Self {
            source: FfeConfigurationSource::Agentless,
            agent_endpoint,
            direct_endpoint,
        }
    }

    /// Reject direct credentials unless their destination is the canonical
    /// HTTPS Event Platform intake for a Datadog site.
    pub fn validate(&self) -> Result<(), String> {
        if self.source != FfeConfigurationSource::Agentless {
            return Ok(());
        }
        if let Some(endpoint) = &self.direct_endpoint {
            validate_direct_endpoint(endpoint)?;
        }
        Ok(())
    }

    /// Validate configuration supplied without a logical SDK identity.
    ///
    /// Agentless delivery is intentionally rejected on this compatibility
    /// path: direct intake must identify the SDK that produced the events,
    /// rather than silently identifying the sidecar process that sent them.
    pub fn validate_without_identity(&self) -> Result<(), String> {
        self.validate()?;
        if self.source == FfeConfigurationSource::Agentless {
            return Err(
                "Agentless EVP configuration requires a logical SDK producer identity".to_owned(),
            );
        }
        Ok(())
    }
}

/// Additive identity-bearing configuration for logical SDK producers.
/// Keeping this separate preserves the existing configuration message layout.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FfeEvpTransportConfigWithIdentity {
    pub transport: FfeEvpTransportConfig,
    pub producer: FfeEvpProducerIdentity,
}

impl FfeEvpTransportConfigWithIdentity {
    pub fn new(
        transport: FfeEvpTransportConfig,
        producer: FfeEvpProducerIdentity,
    ) -> Result<Self, String> {
        transport.validate()?;
        Ok(Self {
            transport,
            producer,
        })
    }
}

fn validate_direct_endpoint(endpoint: &Endpoint) -> Result<(), String> {
    if endpoint.api_key.as_deref().is_none_or(str::is_empty) {
        return Err("direct EVP endpoint requires a non-empty API key".to_owned());
    }
    if endpoint
        .api_key
        .as_deref()
        .is_some_and(|key| http::HeaderValue::try_from(key).is_err())
    {
        return Err("direct EVP endpoint API key is not a valid HTTP header value".to_owned());
    }
    if endpoint.url.scheme_str() != Some("https") {
        return Err("direct EVP endpoint must use HTTPS".to_owned());
    }
    let authority = endpoint
        .url
        .authority()
        .ok_or_else(|| "direct EVP endpoint requires an authority".to_owned())?;
    if authority.as_str().contains('@') || authority.port().is_some() {
        return Err(
            "direct EVP endpoint must use the canonical authority without userinfo or a port"
                .to_owned(),
        );
    }
    let host = authority.host();
    let Some(site) = host.strip_prefix("event-platform-intake.") else {
        return Err("direct EVP endpoint host must be event-platform-intake.<site>".to_owned());
    };
    if !is_valid_dns_site(site) {
        return Err("direct EVP endpoint contains an invalid Datadog site".to_owned());
    }
    let path_and_query = endpoint.url.path_and_query().map(|value| value.as_str());
    if !matches!(path_and_query, None | Some("") | Some("/")) {
        return Err("direct EVP endpoint must not include a path or query".to_owned());
    }
    Ok(())
}

fn is_valid_dns_site(site: &str) -> bool {
    site.len() <= 253
        && site.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProxyVersion {
    V4,
    V2,
}

impl ProxyVersion {
    fn path(self) -> &'static str {
        match self {
            Self::V4 => EVP_PROXY_V4_PATH,
            Self::V2 => EVP_PROXY_V2_PATH,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RouteState {
    Unresolved,
    Local(ProxyVersion),
    Direct,
    Unavailable { retry_at: Instant },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Route {
    Local(ProxyVersion),
    Direct,
}

/// A per-session selector. Clones share route state, allowing exposures and
/// flag evaluations to make one discovery decision while distinct sessions
/// remain isolated even when they target the same intake URL.
#[derive(Clone)]
pub(crate) struct FfeEvpTransport {
    id: u64,
    config: Arc<FfeEvpTransportConfig>,
    producer: Arc<FfeEvpProducerIdentity>,
    state: Arc<AsyncMutex<RouteState>>,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    unavailable_recovery_cooldown: Duration,
}

impl std::fmt::Debug for FfeEvpTransport {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FfeEvpTransport")
            .field("id", &self.id)
            .field("config", &self.config)
            .field("producer", &self.producer)
            .finish_non_exhaustive()
    }
}

impl PartialEq for FfeEvpTransport {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for FfeEvpTransport {}

impl Hash for FfeEvpTransport {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl FfeEvpTransport {
    pub(crate) fn new(config: FfeEvpTransportConfig) -> Result<Self, String> {
        config.validate_without_identity()?;
        Ok(Self::new_with_identity(
            config,
            FfeEvpProducerIdentity::legacy_sidecar(),
        ))
    }

    pub(crate) fn new_with_identity(
        mut config: FfeEvpTransportConfig,
        producer: FfeEvpProducerIdentity,
    ) -> Self {
        // A local receiver must never receive direct-intake credentials, even
        // if a caller accidentally copied them onto the Agent endpoint.
        config.agent_endpoint.api_key = None;

        // Agent-backed FFE never retains direct credentials. Invalid direct
        // configurations are also dropped when received from an untrusted IPC
        // peer; native callers get the validation error before enqueueing.
        if config.source == FfeConfigurationSource::Agent
            || config
                .direct_endpoint
                .as_ref()
                .is_some_and(|endpoint| validate_direct_endpoint(endpoint).is_err())
        {
            config.direct_endpoint = None;
        }

        let state = match config.source {
            FfeConfigurationSource::Agent => RouteState::Local(ProxyVersion::V2),
            FfeConfigurationSource::Agentless => RouteState::Unresolved,
        };

        Self {
            id: NEXT_TRANSPORT_ID.fetch_add(1, Ordering::Relaxed),
            config: Arc::new(config),
            producer: Arc::new(producer),
            state: Arc::new(AsyncMutex::new(state)),
            clock: Arc::new(Instant::now),
            unavailable_recovery_cooldown: DEFAULT_UNAVAILABLE_RECOVERY_COOLDOWN,
        }
    }

    pub(crate) fn agent_only(endpoint: Endpoint) -> Self {
        Self::new_with_identity(
            FfeEvpTransportConfig::agent(endpoint),
            FfeEvpProducerIdentity::legacy_sidecar(),
        )
    }

    #[cfg(test)]
    pub(crate) fn shares_route_state(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    pub(crate) fn producer(&self) -> &FfeEvpProducerIdentity {
        &self.producer
    }

    pub(crate) fn deduplication_scope(&self) -> String {
        format!(
            "{}\0{}\0{}",
            self.id,
            self.producer.origin(),
            self.producer.version()
        )
    }

    #[cfg(test)]
    fn with_clock(
        mut self,
        clock: Arc<dyn Fn() -> Instant + Send + Sync>,
        unavailable_recovery_cooldown: Duration,
    ) -> Self {
        self.clock = clock;
        self.unavailable_recovery_cooldown = unavailable_recovery_cooldown;
        self
    }

    /// Send one already-encoded JSON payload through the selected route.
    ///
    /// A local 404/405 or a definitive pre-send failure replays the current
    /// payload directly. Ambiguous failures switch only future payloads. Other
    /// statuses, including 429 and 5xx, leave the local route unchanged.
    pub(crate) async fn send_payload<C: HttpClientCapability + SleepCapability>(
        &self,
        client: &C,
        intake_path: &'static str,
        payload: String,
        log_prefix: &'static str,
        success_name: &'static str,
    ) -> bool {
        let Some(route) = self.resolve_route(client, log_prefix).await else {
            debug!("{log_prefix}: no compatible EVP route is available");
            return false;
        };

        let first = self
            .send_once(
                client,
                route,
                intake_path,
                payload.clone(),
                log_prefix,
                success_name,
            )
            .await;
        let Err(failure) = first else {
            return true;
        };

        if route == Route::Direct {
            log_failure(log_prefix, &failure);
            return false;
        }

        let replay = match &failure {
            DeliveryFailure::DefinitivePreSend(_) => true,
            DeliveryFailure::Status(status) => AGENT_ROUTE_REJECTION_STATUSES.contains(status),
            DeliveryFailure::Ambiguous(_) => false,
        };
        let switch_future = replay || matches!(&failure, DeliveryFailure::Ambiguous(_));

        if switch_future && self.leave_local_route().await && replay {
            let direct = self
                .send_once(
                    client,
                    Route::Direct,
                    intake_path,
                    payload,
                    log_prefix,
                    success_name,
                )
                .await;
            if let Err(direct_failure) = direct {
                log_failure(log_prefix, &direct_failure);
                return false;
            }
            return true;
        }

        log_failure(log_prefix, &failure);
        false
    }

    async fn resolve_route<C: HttpClientCapability + SleepCapability>(
        &self,
        client: &C,
        log_prefix: &'static str,
    ) -> Option<Route> {
        let mut state = self.state.lock().await;
        match *state {
            RouteState::Local(version) => return Some(Route::Local(version)),
            RouteState::Direct => return Some(Route::Direct),
            RouteState::Unavailable { retry_at } if (self.clock)() < retry_at => return None,
            RouteState::Unavailable { .. } => {}
            RouteState::Unresolved => {}
        }

        // Discovery is intentionally serialized under the route lock so both
        // event writers share exactly one first-send probe and decision.
        if let Some(version) = self.discover_local_route(client, log_prefix).await {
            *state = RouteState::Local(version);
            return Some(Route::Local(version));
        }

        if self.config.direct_endpoint.is_some() {
            *state = RouteState::Direct;
            return Some(Route::Direct);
        }

        *state = RouteState::Unavailable {
            retry_at: (self.clock)() + self.unavailable_recovery_cooldown,
        };
        None
    }

    async fn discover_local_route<C: HttpClientCapability + SleepCapability>(
        &self,
        client: &C,
        log_prefix: &'static str,
    ) -> Option<ProxyVersion> {
        let endpoint = agent_endpoint_with_path(&self.config.agent_endpoint, INFO_PATH).ok()?;
        let request = endpoint
            .to_request_builder(USER_AGENT)
            .ok()?
            .method(Method::GET)
            .body(Bytes::new())
            .ok()?;
        let timeout = Duration::from_millis(endpoint.timeout_ms);
        let response = tokio::select! {
            biased;
            result = client.request(request) => result.ok()?,
            _ = client.sleep(timeout) => {
                debug!("{log_prefix}: Agent /info probe timed out after {timeout:?}");
                return None;
            }
        };

        if !response.status().is_success() || response.body().len() > MAX_INFO_RESPONSE_BYTES {
            return None;
        }

        #[derive(Deserialize)]
        struct InfoResponse {
            #[serde(default)]
            endpoints: Vec<String>,
            evp_proxy_allowed_headers: Option<Vec<String>>,
        }

        let info: InfoResponse = serde_json::from_slice(response.body()).ok()?;
        if !supports_identity_headers(info.evp_proxy_allowed_headers.as_deref()) {
            return None;
        }
        select_proxy_version(&info.endpoints)
    }

    async fn leave_local_route(&self) -> bool {
        if self.config.source != FfeConfigurationSource::Agentless {
            return false;
        }

        let mut state = self.state.lock().await;
        if self.config.direct_endpoint.is_some() {
            *state = RouteState::Direct;
            true
        } else {
            *state = RouteState::Unavailable {
                retry_at: (self.clock)() + self.unavailable_recovery_cooldown,
            };
            false
        }
    }

    async fn send_once<C: HttpClientCapability + SleepCapability>(
        &self,
        client: &C,
        route: Route,
        intake_path: &'static str,
        payload: String,
        log_prefix: &'static str,
        success_name: &'static str,
    ) -> Result<(), DeliveryFailure> {
        let endpoint = match route {
            Route::Local(version) => agent_endpoint_with_path(
                &self.config.agent_endpoint,
                &join_paths(version.path(), intake_path),
            ),
            Route::Direct => self
                .config
                .direct_endpoint
                .as_ref()
                .ok_or_else(|| "direct endpoint is not configured".to_owned())
                .and_then(|base| endpoint_with_path(base, intake_path)),
        }
        .map_err(DeliveryFailure::DefinitivePreSend)?;

        let mut builder = endpoint
            .to_request_builder(USER_AGENT)
            .map_err(|error| DeliveryFailure::DefinitivePreSend(error.to_string()))?
            .method(Method::POST)
            .header("Content-Type", "application/json")
            .header(EVP_ORIGIN_HEADER, self.producer.origin())
            .header(EVP_ORIGIN_VERSION_HEADER, self.producer.version());
        if matches!(route, Route::Local(_)) {
            builder = builder.header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE);
        }
        let request = builder
            .body(Bytes::from(payload))
            .map_err(|error| DeliveryFailure::DefinitivePreSend(error.to_string()))?;

        let timeout = Duration::from_millis(endpoint.timeout_ms);
        let response = tokio::select! {
            biased;
            result = client.request(request) => result.map_err(classify_http_error)?,
            _ = client.sleep(timeout) => {
                return Err(DeliveryFailure::Ambiguous(format!(
                    "request timed out after {timeout:?}"
                )));
            }
        };

        let status = response.status();
        if !status.is_success() {
            return Err(DeliveryFailure::Status(status.as_u16()));
        }

        debug!("{log_prefix}: sent {success_name}, status={status}");
        Ok(())
    }
}

fn endpoint_with_path(base: &Endpoint, path: &str) -> Result<Endpoint, String> {
    let path = PathAndQuery::try_from(path).map_err(|error| error.to_string())?;
    let mut parts = base.url.clone().into_parts();
    parts.path_and_query = Some(path);
    let url = http::Uri::from_parts(parts).map_err(|error| error.to_string())?;
    Ok(Endpoint {
        url,
        ..base.clone()
    })
}

fn agent_endpoint_with_path(base: &Endpoint, path: &str) -> Result<Endpoint, String> {
    let base_path = base.url.path().trim_end_matches('/');
    let prefix = TRACE_ENDPOINT_PATHS
        .iter()
        .find_map(|trace_path| base_path.strip_suffix(trace_path))
        .unwrap_or(base_path);
    endpoint_with_path(base, &join_paths(prefix, path))
}

fn supports_identity_headers(headers: Option<&[String]>) -> bool {
    let Some(headers) = headers else {
        return false;
    };
    [EVP_ORIGIN_HEADER, EVP_ORIGIN_VERSION_HEADER]
        .iter()
        .all(|required| {
            headers
                .iter()
                .any(|header| header.trim().eq_ignore_ascii_case(required))
        })
}

fn select_proxy_version(endpoints: &[String]) -> Option<ProxyVersion> {
    for (path, version) in [
        (EVP_PROXY_V4_PATH, ProxyVersion::V4),
        (EVP_PROXY_V2_PATH, ProxyVersion::V2),
    ] {
        if endpoints
            .iter()
            .any(|endpoint| endpoint.trim_end_matches('/') == path)
        {
            return Some(version);
        }
    }
    None
}

fn join_paths(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[derive(Debug)]
enum DeliveryFailure {
    DefinitivePreSend(String),
    Ambiguous(String),
    Status(u16),
}

fn classify_http_error(error: HttpError) -> DeliveryFailure {
    match error {
        HttpError::InvalidRequest(error) => DeliveryFailure::DefinitivePreSend(error.to_string()),
        HttpError::Network(error) if is_definitive_connection_failure(&error) => {
            DeliveryFailure::DefinitivePreSend(error.to_string())
        }
        HttpError::Network(error) | HttpError::ResponseBody(error) | HttpError::Other(error) => {
            DeliveryFailure::Ambiguous(error.to_string())
        }
        HttpError::Timeout => DeliveryFailure::Ambiguous("request timed out".to_owned()),
    }
}

fn is_definitive_connection_failure(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
            error.raw_os_error() == Some(WSAECONNREFUSED)
                || matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::AddrNotAvailable
                )
        })
    })
}

fn log_failure(log_prefix: &'static str, failure: &DeliveryFailure) {
    match failure {
        DeliveryFailure::Status(status) => warn!("{log_prefix}: non-2xx response {status}"),
        DeliveryFailure::DefinitivePreSend(error) | DeliveryFailure::Ambiguous(error) => {
            debug!("{log_prefix}: request failed: {error}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_capabilities::MaybeSend;
    use std::collections::VecDeque;
    use std::future;
    use std::sync::Mutex;

    #[derive(Clone, Debug)]
    struct ScriptedCapabilities {
        inner: Arc<Mutex<Script>>,
    }

    #[derive(Debug, Default)]
    struct Script {
        responses: VecDeque<Result<http::Response<Bytes>, HttpError>>,
        requests: Vec<http::Request<Bytes>>,
    }

    impl ScriptedCapabilities {
        fn new(responses: Vec<Result<http::Response<Bytes>, HttpError>>) -> Self {
            Self {
                inner: Arc::new(Mutex::new(Script {
                    responses: responses.into(),
                    requests: Vec::new(),
                })),
            }
        }

        fn requests(&self) -> Vec<(String, http::HeaderMap)> {
            self.inner
                .lock()
                .unwrap()
                .requests
                .iter()
                .map(|request| (request.uri().to_string(), request.headers().clone()))
                .collect()
        }
    }

    impl HttpClientCapability for ScriptedCapabilities {
        fn new_client() -> Self {
            Self::new(Vec::new())
        }

        fn new_without_connection_pooling() -> Self {
            Self::new(Vec::new())
        }

        fn request(
            &self,
            request: http::Request<Bytes>,
        ) -> impl future::Future<Output = Result<http::Response<Bytes>, HttpError>> + MaybeSend
        {
            let inner = self.inner.clone();
            async move {
                let mut script = inner.lock().unwrap();
                script.requests.push(request);
                script
                    .responses
                    .pop_front()
                    .expect("missing scripted HTTP response")
            }
        }
    }

    impl SleepCapability for ScriptedCapabilities {
        fn new() -> Self {
            Self::new(Vec::new())
        }

        async fn sleep(&self, _duration: Duration) {
            future::pending::<()>().await
        }
    }

    fn response(status: u16, body: &str) -> Result<http::Response<Bytes>, HttpError> {
        Ok(http::Response::builder()
            .status(status)
            .body(Bytes::copy_from_slice(body.as_bytes()))
            .unwrap())
    }

    fn info_response(endpoints: &[&str]) -> Result<http::Response<Bytes>, HttpError> {
        response(
            200,
            &serde_json::json!({
                "endpoints": endpoints,
                "evp_proxy_allowed_headers": [
                    EVP_ORIGIN_HEADER,
                    EVP_ORIGIN_VERSION_HEADER,
                ],
            })
            .to_string(),
        )
    }

    fn endpoint(url: &str, api_key: Option<&'static str>) -> Endpoint {
        Endpoint {
            url: url.parse().unwrap(),
            api_key: api_key.map(Into::into),
            ..Endpoint::default()
        }
    }

    fn agentless(direct_key: Option<&'static str>) -> FfeEvpTransport {
        FfeEvpTransport::new_with_identity(
            FfeEvpTransportConfig::agentless(
                endpoint(
                    "http://agent.internal:8126/v0.4/traces",
                    Some("must-not-leak"),
                ),
                direct_key
                    .map(|key| endpoint("https://event-platform-intake.datadoghq.com/", Some(key))),
            ),
            FfeEvpProducerIdentity::new("dd-trace-rb", "3.0.0").unwrap(),
        )
    }

    async fn send(
        transport: &FfeEvpTransport,
        client: &ScriptedCapabilities,
        path: &'static str,
    ) -> bool {
        transport
            .send_payload(client, path, "{}".to_owned(), "test", "batch")
            .await
    }

    #[tokio::test]
    async fn concurrent_first_flush_prefers_v4_and_performs_one_shared_discovery() {
        let client = ScriptedCapabilities::new(vec![
            info_response(&["/evp_proxy/v2/", "/evp_proxy/v4"]),
            response(202, ""),
            response(202, ""),
        ]);
        let transport = agentless(Some("api-key"));
        let flag_transport = transport.clone();
        assert!(transport.shares_route_state(&flag_transport));

        let (exposure_sent, evaluation_sent) = tokio::join!(
            send(&transport, &client, "/api/v2/exposures"),
            send(&flag_transport, &client, "/api/v2/flagevaluation")
        );
        assert!(exposure_sent);
        assert!(evaluation_sent);

        let requests = client.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|(url, _)| url == "http://agent.internal:8126/info")
                .count(),
            1
        );
        assert!(requests
            .iter()
            .any(|(url, _)| url == "http://agent.internal:8126/evp_proxy/v4/api/v2/exposures"));
        assert!(
            requests
                .iter()
                .any(|(url, _)| url
                    == "http://agent.internal:8126/evp_proxy/v4/api/v2/flagevaluation")
        );
        for (_, headers) in &requests[1..] {
            assert_eq!(headers.get("user-agent").unwrap(), USER_AGENT);
            assert_eq!(
                headers.get(EVP_SUBDOMAIN_HEADER).unwrap(),
                EVP_SUBDOMAIN_VALUE
            );
            assert!(!headers.contains_key("dd-api-key"));
            assert_eq!(headers.get(EVP_ORIGIN_HEADER).unwrap(), "dd-trace-rb");
            assert_eq!(headers.get(EVP_ORIGIN_VERSION_HEADER).unwrap(), "3.0.0");
        }
    }

    #[tokio::test]
    async fn v2_is_used_when_v4_is_not_advertised() {
        let client =
            ScriptedCapabilities::new(vec![info_response(&["/evp_proxy/v2"]), response(202, "")]);
        let transport = agentless(Some("api-key"));

        assert!(send(&transport, &client, "/api/v2/exposures").await);
        assert_eq!(
            client.requests()[1].0,
            "http://agent.internal:8126/evp_proxy/v2/api/v2/exposures"
        );
    }

    #[tokio::test]
    async fn discovery_requires_both_identity_headers() {
        for body in [
            r#"{"endpoints":["/evp_proxy/v4"]}"#,
            r#"{"endpoints":["/evp_proxy/v4"],"evp_proxy_allowed_headers":null}"#,
            r#"{"endpoints":["/evp_proxy/v4"],"evp_proxy_allowed_headers":["DD-EVP-ORIGIN"]}"#,
            r#"{"endpoints":["/evp_proxy/v4"],"evp_proxy_allowed_headers":["DD-EVP-ORIGIN-VERSION"]}"#,
        ] {
            let client = ScriptedCapabilities::new(vec![response(200, body), response(202, "")]);
            let transport = agentless(Some("api-key"));

            assert!(send(&transport, &client, "/api/v2/exposures").await);
            assert_eq!(
                client.requests()[1].0,
                "https://event-platform-intake.datadoghq.com/api/v2/exposures",
                "accepted incomplete Agent identity-header capabilities from {body}",
            );
        }
    }

    #[tokio::test]
    async fn prefixed_agent_discovery_and_v4_delivery_preserve_prefix() {
        let client = ScriptedCapabilities::new(vec![
            response(
                200,
                r#"{"endpoints":["/evp_proxy/v4"],"evp_proxy_allowed_headers":[" dd-evp-origin ","\tDd-EvP-OrIgIn-VeRsIoN\t"]}"#,
            ),
            response(202, ""),
        ]);
        let transport = FfeEvpTransport::new_with_identity(
            FfeEvpTransportConfig::agentless(
                endpoint(
                    "http://agent.internal:8126/customer/proxy/v0.4/traces",
                    None,
                ),
                Some(endpoint(
                    "https://event-platform-intake.datadoghq.com/",
                    Some("api-key"),
                )),
            ),
            FfeEvpProducerIdentity::new("dd-trace-rb", "3.0.0").unwrap(),
        );

        assert!(send(&transport, &client, "/api/v2/exposures").await);
        let requests = client.requests();
        assert_eq!(
            requests[0].0,
            "http://agent.internal:8126/customer/proxy/info"
        );
        assert_eq!(
            requests[1].0,
            "http://agent.internal:8126/customer/proxy/evp_proxy/v4/api/v2/exposures"
        );
    }

    #[tokio::test]
    async fn missing_local_route_selects_authenticated_direct_and_stays_sticky() {
        let client = ScriptedCapabilities::new(vec![
            info_response(&[]),
            response(202, ""),
            response(202, ""),
        ]);
        let transport = agentless(Some("first-key"));

        assert!(send(&transport, &client, "/api/v2/exposures").await);
        assert!(send(&transport, &client, "/api/v2/flagevaluation").await);

        let requests = client.requests();
        assert_eq!(requests.len(), 3, "direct selection must not re-probe");
        assert!(!requests[0].1.contains_key("dd-api-key"));
        for (url, headers) in &requests[1..] {
            assert!(url.starts_with("https://event-platform-intake.datadoghq.com/api/v2/"));
            assert_eq!(headers.get("user-agent").unwrap(), USER_AGENT);
            assert_eq!(headers.get("dd-api-key").unwrap(), "first-key");
            assert!(!headers.contains_key(EVP_SUBDOMAIN_HEADER));
            assert_eq!(headers.get(EVP_ORIGIN_HEADER).unwrap(), "dd-trace-rb");
            assert_eq!(headers.get(EVP_ORIGIN_VERSION_HEADER).unwrap(), "3.0.0");
        }
    }

    #[tokio::test]
    async fn discovery_failure_selects_direct() {
        let client = ScriptedCapabilities::new(vec![
            Err(HttpError::Network(anyhow::anyhow!("unreachable"))),
            response(202, ""),
        ]);
        let transport = agentless(Some("api-key"));

        assert!(send(&transport, &client, "/api/v2/exposures").await);
        assert!(client.requests()[1]
            .0
            .starts_with("https://event-platform-intake.datadoghq.com/"));
    }

    #[tokio::test]
    async fn rejected_local_route_replays_current_batch_direct() {
        for status in [404, 405] {
            let client = ScriptedCapabilities::new(vec![
                info_response(&["/evp_proxy/v4"]),
                response(status, "rejected"),
                response(202, ""),
                response(202, ""),
            ]);
            let transport = agentless(Some("api-key"));

            assert!(send(&transport, &client, "/api/v2/exposures").await);
            assert!(send(&transport, &client, "/api/v2/flagevaluation").await);

            let requests = client.requests();
            assert!(requests[1].0.contains("/evp_proxy/v4/"));
            assert!(requests[2].0.starts_with("https://event-platform-intake."));
            assert!(requests[3].0.starts_with("https://event-platform-intake."));
        }
    }

    #[tokio::test]
    async fn forbidden_local_response_does_not_replay_or_change_routes() {
        let client = ScriptedCapabilities::new(vec![
            info_response(&["/evp_proxy/v4"]),
            response(403, "forbidden"),
            response(202, ""),
        ]);
        let transport = agentless(Some("api-key"));

        assert!(!send(&transport, &client, "/api/v2/exposures").await);
        assert!(send(&transport, &client, "/api/v2/flagevaluation").await);

        let requests = client.requests();
        assert_eq!(requests.len(), 3, "403 response was replayed");
        assert!(requests[1].0.contains("/evp_proxy/v4/"));
        assert!(requests[2].0.contains("/evp_proxy/v4/"));
    }

    #[tokio::test]
    async fn definitive_pre_send_failure_replays_current_batch() {
        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        let client = ScriptedCapabilities::new(vec![
            info_response(&["/evp_proxy/v2"]),
            Err(HttpError::Network(anyhow::Error::new(refused))),
            response(202, ""),
        ]);
        let transport = agentless(Some("api-key"));

        assert!(send(&transport, &client, "/api/v2/exposures").await);
        let requests = client.requests();
        assert!(requests[1].0.contains("/evp_proxy/v2/"));
        assert!(requests[2].0.starts_with("https://event-platform-intake."));
    }

    #[tokio::test]
    async fn ambiguous_failure_changes_only_future_routing() {
        let reset = std::io::Error::from(std::io::ErrorKind::ConnectionReset);
        let client = ScriptedCapabilities::new(vec![
            info_response(&["/evp_proxy/v2"]),
            Err(HttpError::Network(anyhow::Error::new(reset))),
            response(202, ""),
        ]);
        let transport = agentless(Some("api-key"));

        assert!(!send(&transport, &client, "/api/v2/exposures").await);
        assert_eq!(client.requests().len(), 2, "ambiguous batch was replayed");
        assert!(send(&transport, &client, "/api/v2/flagevaluation").await);
        assert!(client.requests()[2]
            .0
            .starts_with("https://event-platform-intake."));
    }

    #[tokio::test]
    async fn overload_and_server_failures_do_not_switch_routes() {
        for status in [429, 500] {
            let client = ScriptedCapabilities::new(vec![
                info_response(&["/evp_proxy/v2"]),
                response(status, "try later"),
                response(202, ""),
            ]);
            let transport = agentless(Some("api-key"));

            assert!(!send(&transport, &client, "/api/v2/exposures").await);
            assert!(send(&transport, &client, "/api/v2/flagevaluation").await);
            assert!(client.requests()[1].0.contains("/evp_proxy/v2/"));
            assert!(client.requests()[2].0.contains("/evp_proxy/v2/"));
        }
    }

    #[tokio::test]
    async fn unavailable_route_reprobes_once_after_cooldown_and_recovers() {
        let client = ScriptedCapabilities::new(vec![
            info_response(&[]),
            info_response(&["/evp_proxy/v4"]),
            response(202, ""),
            response(202, ""),
        ]);
        let current = Arc::new(Mutex::new(Instant::now()));
        let clock_state = current.clone();
        let cooldown = Duration::from_secs(30);
        let transport =
            agentless(None).with_clock(Arc::new(move || *clock_state.lock().unwrap()), cooldown);

        assert!(!send(&transport, &client, "/api/v2/exposures").await);
        assert!(!send(&transport, &client, "/api/v2/flagevaluation").await);
        assert_eq!(
            client.requests().len(),
            1,
            "unavailable route re-probed early"
        );

        {
            let mut now = current.lock().unwrap();
            *now += cooldown;
        }
        let other_writer = transport.clone();
        let (exposure_sent, evaluation_sent) = tokio::join!(
            send(&transport, &client, "/api/v2/exposures"),
            send(&other_writer, &client, "/api/v2/flagevaluation")
        );
        assert!(exposure_sent);
        assert!(evaluation_sent);

        let requests = client.requests();
        assert_eq!(
            requests
                .iter()
                .filter(|(url, _)| url == "http://agent.internal:8126/info")
                .count(),
            2,
            "concurrent recovery performed more than one new discovery"
        );
        assert!(requests[2..]
            .iter()
            .all(|(url, _)| url.contains("/evp_proxy/v4/")));
    }

    #[tokio::test]
    async fn agent_only_mode_keeps_historical_v2_without_discovery_or_fallback() {
        for first_failure in [
            response(404, "missing"),
            Err(HttpError::Network(anyhow::Error::new(
                std::io::Error::from(std::io::ErrorKind::ConnectionReset),
            ))),
        ] {
            let client = ScriptedCapabilities::new(vec![first_failure, response(202, "")]);
            let transport = FfeEvpTransport::agent_only(endpoint(
                "http://agent.internal:8126/v0.4/traces",
                Some("must-not-leak"),
            ));

            assert!(!send(&transport, &client, "/api/v2/exposures").await);
            assert!(send(&transport, &client, "/api/v2/exposures").await);
            let requests = client.requests();
            assert_eq!(requests.len(), 2);
            assert!(requests
                .iter()
                .all(|(url, _)| url == "http://agent.internal:8126/evp_proxy/v2/api/v2/exposures"));
            assert!(requests
                .iter()
                .all(|(_, headers)| !headers.contains_key("dd-api-key")));
            assert_eq!(
                requests[0].1.get(EVP_ORIGIN_HEADER).unwrap(),
                "ddtrace-sidecar"
            );
            assert_eq!(
                requests[0].1.get(EVP_ORIGIN_VERSION_HEADER).unwrap(),
                crate::sidecar_version!()
            );
        }
    }

    #[tokio::test]
    async fn agent_only_v2_delivery_preserves_agent_path_prefix() {
        let client = ScriptedCapabilities::new(vec![response(202, "")]);
        let transport = FfeEvpTransport::agent_only(endpoint(
            "http://agent.internal:8126/customer/proxy/v0.4/traces",
            None,
        ));

        assert!(send(&transport, &client, "/api/v2/exposures").await);
        assert_eq!(
            client.requests()[0].0,
            "http://agent.internal:8126/customer/proxy/evp_proxy/v2/api/v2/exposures"
        );
    }

    #[tokio::test]
    async fn redirect_response_is_not_followed_or_replayed() {
        let client = ScriptedCapabilities::new(vec![info_response(&[]), response(302, "redirect")]);
        let transport = agentless(Some("api-key"));

        assert!(!send(&transport, &client, "/api/v2/exposures").await);
        let requests = client.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].0, "http://agent.internal:8126/info");
        assert_eq!(
            requests[1].0,
            "https://event-platform-intake.datadoghq.com/api/v2/exposures"
        );
    }

    #[tokio::test]
    async fn distinct_sessions_keep_direct_credentials_isolated() {
        let client = ScriptedCapabilities::new(vec![
            info_response(&[]),
            response(202, ""),
            info_response(&[]),
            response(202, ""),
        ]);
        let first = agentless(Some("first-key"));
        let second = agentless(Some("second-key"));
        assert!(!first.shares_route_state(&second));

        assert!(send(&first, &client, "/api/v2/exposures").await);
        assert!(send(&second, &client, "/api/v2/exposures").await);

        let requests = client.requests();
        assert_eq!(requests[1].1.get("dd-api-key").unwrap(), "first-key");
        assert_eq!(requests[3].1.get("dd-api-key").unwrap(), "second-key");
    }

    #[test]
    fn direct_endpoint_validation_rejects_credential_exfiltration_targets() {
        for url in [
            "http://event-platform-intake.datadoghq.com/",
            "https://example.com/",
            "https://event-platform-intake.datadoghq.com:8443/",
            "https://event-platform-intake.datadoghq.com/unexpected",
            "https://event-platform-intake.datadoghq.com/?query=1",
            "https://event-platform-intake.-datadoghq.com/",
            "https://event-platform-intake.datadoghq-.com/",
            "https://event-platform-intake.data_doghq.com/",
            "https://event-platform-intake.DATADOGHQ.COM/",
        ] {
            let config = FfeEvpTransportConfig::agentless(
                endpoint("http://agent.internal:8126/", None),
                Some(endpoint(url, Some("secret"))),
            );
            assert!(config.validate().is_err(), "accepted direct URL {url}");
        }

        assert!(FfeEvpTransportConfig::agentless(
            endpoint("http://agent.internal:8126/", None),
            Some(endpoint(
                "https://event-platform-intake.datadoghq.eu/",
                Some("secret")
            )),
        )
        .validate()
        .is_ok());
    }

    #[test]
    fn identityless_transport_rejects_agentless_configuration() {
        let config = FfeEvpTransportConfig::agentless(
            endpoint("http://agent.internal:8126/v0.4/traces", None),
            Some(endpoint(
                "https://event-platform-intake.datadoghq.com/",
                Some("api-key"),
            )),
        );
        assert!(config.validate().is_ok());
        assert_eq!(
            config.validate_without_identity().unwrap_err(),
            "Agentless EVP configuration requires a logical SDK producer identity"
        );
        assert!(FfeEvpTransport::new(config).is_err());
    }

    #[test]
    fn definitive_connection_failure_recognizes_wrapped_platform_sentinels() {
        let windows_refused =
            anyhow::Error::new(std::io::Error::from_raw_os_error(WSAECONNREFUSED))
                .context("connect failed");
        assert!(is_definitive_connection_failure(&windows_refused));

        let missing_socket = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::NotFound))
            .context("connect failed");
        assert!(is_definitive_connection_failure(&missing_socket));

        let reset = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::ConnectionReset))
            .context("write failed");
        assert!(!is_definitive_connection_failure(&reset));
    }

    #[test]
    fn producer_identity_rejects_untrusted_header_values_and_wire_input() {
        assert!(FfeEvpProducerIdentity::new("", "1.0.0").is_err());
        assert!(FfeEvpProducerIdentity::new("dd-trace-rb", "invalid\nversion").is_err());
        assert!(FfeEvpProducerIdentity::new(
            "x".repeat(MAX_FFE_EVP_PRODUCER_IDENTITY_LENGTH + 1),
            "1.0.0"
        )
        .is_err());

        #[derive(Serialize)]
        struct WireIdentity<'a> {
            origin: &'a str,
            version: &'a str,
        }
        let bytes = bincode::serialize(&WireIdentity {
            origin: "invalid\norigin",
            version: "1.0.0",
        })
        .unwrap();
        assert!(bincode::deserialize::<FfeEvpProducerIdentity>(&bytes).is_err());
    }
}
