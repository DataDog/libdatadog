// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared EVP route selection and transport.
//!
//! Consumers remain on the historical fixed Agent EVP v2 route unless they
//! explicitly opt into local discovery and authenticated direct fallback.

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

pub(crate) use evp_proxy::SUBDOMAIN_HEADER as EVP_SUBDOMAIN_HEADER;
#[cfg(test)]
const EVP_SUBDOMAIN_VALUE: &str = evp_proxy::EVENT_PLATFORM_INTAKE_SUBDOMAIN;

const USER_AGENT: &str = concat!("ddtrace-sidecar/", crate::sidecar_version!());
const EVP_PROXY_V4_PATH: &str = "/evp_proxy/v4";
const EVP_PROXY_V2_PATH: &str = "/evp_proxy/v2";
const INFO_PATH: &str = "/info";
const TRACE_ENDPOINT_PATHS: &[&str] = &["/v0.4/traces", "/v0.5/traces", "/v1.0/traces"];
const MAX_INFO_RESPONSE_BYTES: usize = 1 << 20;
const EVP_ORIGIN_HEADER: &str = "DD-EVP-ORIGIN";
const EVP_ORIGIN_VERSION_HEADER: &str = "DD-EVP-ORIGIN-VERSION";
const DEFAULT_UNAVAILABLE_RECOVERY_COOLDOWN: Duration = Duration::from_secs(30);
pub const MAX_EVP_PRODUCER_IDENTITY_LENGTH: usize = 256;
const MAX_DNS_HOST_LENGTH: usize = 253;
// The Agent contract guarantees that these local responses reject the request
// before processing it, so replaying the same batch through direct intake is
// safe. An upstream 403 does not provide that guarantee.
const AGENT_ROUTE_REJECTION_STATUSES: &[u16] = &[404, 405];
const WSAECONNREFUSED: i32 = 10061;

static NEXT_TRANSPORT_ID: AtomicU64 = AtomicU64::new(1);

/// Logical SDK identity attached to EVP requests.
///
/// This is deliberately distinct from the sidecar process identity: intake
/// needs to know which tracer produced the events, not which helper sent them.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct EvpProducerIdentity {
    origin: String,
    version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvpProducerIdentityError {
    EmptyOrigin,
    EmptyVersion,
    OriginTooLong { length: usize },
    VersionTooLong { length: usize },
    InvalidOrigin,
    InvalidVersion,
}

impl Display for EvpProducerIdentityError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyOrigin => formatter.write_str("EVP producer origin must not be empty"),
            Self::EmptyVersion => formatter.write_str("EVP producer version must not be empty"),
            Self::OriginTooLong { length } => write!(
                formatter,
                "EVP producer origin is {length} bytes; maximum is {MAX_EVP_PRODUCER_IDENTITY_LENGTH}"
            ),
            Self::VersionTooLong { length } => write!(
                formatter,
                "EVP producer version is {length} bytes; maximum is {MAX_EVP_PRODUCER_IDENTITY_LENGTH}"
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

impl std::error::Error for EvpProducerIdentityError {}

impl EvpProducerIdentity {
    pub fn new(
        origin: impl Into<String>,
        version: impl Into<String>,
    ) -> Result<Self, EvpProducerIdentityError> {
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

fn validate_identity_field(value: &str, is_origin: bool) -> Result<(), EvpProducerIdentityError> {
    if value.trim().is_empty() {
        return Err(if is_origin {
            EvpProducerIdentityError::EmptyOrigin
        } else {
            EvpProducerIdentityError::EmptyVersion
        });
    }
    let length = value.len();
    if length > MAX_EVP_PRODUCER_IDENTITY_LENGTH {
        return Err(if is_origin {
            EvpProducerIdentityError::OriginTooLong { length }
        } else {
            EvpProducerIdentityError::VersionTooLong { length }
        });
    }
    if value.trim() != value || http::HeaderValue::try_from(value).is_err() {
        return Err(if is_origin {
            EvpProducerIdentityError::InvalidOrigin
        } else {
            EvpProducerIdentityError::InvalidVersion
        });
    }
    Ok(())
}

impl<'de> Deserialize<'de> for EvpProducerIdentity {
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

/// Routing policy selected by an EVP consumer.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum EvpTransportMode {
    /// Preserve the historical fixed Agent EVP v2 route.
    AgentOnly,
    /// Prefer a compatible local EVP route, then fall back to direct intake.
    PreferLocalThenDirect,
}

/// Explicit per-session EVP configuration crossing the sidecar IPC boundary.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvpTransportConfig {
    pub mode: EvpTransportMode,
    pub agent_endpoint: Endpoint,
    pub direct_endpoint: Option<Endpoint>,
    pub intake_subdomain: String,
}

impl EvpTransportConfig {
    pub fn agent_only(agent_endpoint: Endpoint, intake_subdomain: impl Into<String>) -> Self {
        Self {
            mode: EvpTransportMode::AgentOnly,
            agent_endpoint,
            direct_endpoint: None,
            intake_subdomain: intake_subdomain.into(),
        }
    }

    pub fn prefer_local_then_direct(
        agent_endpoint: Endpoint,
        direct_endpoint: Option<Endpoint>,
        intake_subdomain: impl Into<String>,
    ) -> Self {
        Self {
            mode: EvpTransportMode::PreferLocalThenDirect,
            agent_endpoint,
            direct_endpoint,
            intake_subdomain: intake_subdomain.into(),
        }
    }

    /// Validate the target and reject direct credentials unless their
    /// destination matches the configured canonical HTTPS intake.
    pub fn validate(&self) -> Result<(), String> {
        validate_intake_subdomain(&self.intake_subdomain)?;
        if self.mode != EvpTransportMode::PreferLocalThenDirect {
            return Ok(());
        }
        if let Some(endpoint) = &self.direct_endpoint {
            validate_direct_endpoint(endpoint, &self.intake_subdomain)?;
        }
        Ok(())
    }
}

/// Identity-bearing configuration used when a client explicitly configures
/// the shared EVP transport.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvpTransportConfigWithIdentity {
    pub transport: EvpTransportConfig,
    pub producer: EvpProducerIdentity,
}

impl EvpTransportConfigWithIdentity {
    pub fn new(
        transport: EvpTransportConfig,
        producer: EvpProducerIdentity,
    ) -> Result<Self, String> {
        transport.validate()?;
        Ok(Self {
            transport,
            producer,
        })
    }
}

fn validate_direct_endpoint(endpoint: &Endpoint, intake_subdomain: &str) -> Result<(), String> {
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
    let expected_prefix = format!("{intake_subdomain}.");
    let Some(site) = host.strip_prefix(&expected_prefix) else {
        return Err(format!(
            "direct EVP endpoint host must be {intake_subdomain}.<site>"
        ));
    };
    if host.len() > MAX_DNS_HOST_LENGTH || !is_valid_dns_site(site) {
        return Err("direct EVP endpoint contains an invalid Datadog site".to_owned());
    }
    let path_and_query = endpoint.url.path_and_query().map(|value| value.as_str());
    if !matches!(path_and_query, None | Some("") | Some("/")) {
        return Err("direct EVP endpoint must not include a path or query".to_owned());
    }
    Ok(())
}

fn validate_intake_subdomain(subdomain: &str) -> Result<(), String> {
    let valid = !subdomain.is_empty()
        && subdomain.len() <= 63
        && subdomain
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && subdomain
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && subdomain
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric);
    if valid {
        Ok(())
    } else {
        Err("EVP intake subdomain must be one canonical DNS label".to_owned())
    }
}

fn is_valid_dns_site(site: &str) -> bool {
    site.len() <= MAX_DNS_HOST_LENGTH
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

struct EvpRequest<'a> {
    intake_path: &'a str,
    content_type: &'a str,
    payload: Bytes,
    log_prefix: &'a str,
    success_name: &'a str,
}

/// A per-session selector. Clones share route state, allowing exposures and
/// flag evaluations to make one discovery decision while distinct sessions
/// remain isolated even when they target the same intake URL.
#[derive(Clone)]
pub(crate) struct EvpTransport {
    id: u64,
    config: Arc<EvpTransportConfig>,
    producer: Arc<EvpProducerIdentity>,
    state: Arc<AsyncMutex<RouteState>>,
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
    unavailable_recovery_cooldown: Duration,
}

impl std::fmt::Debug for EvpTransport {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EvpTransport")
            .field("id", &self.id)
            .field("config", &self.config)
            .field("producer", &self.producer)
            .finish_non_exhaustive()
    }
}

impl PartialEq for EvpTransport {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for EvpTransport {}

impl Hash for EvpTransport {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl EvpTransport {
    pub(crate) fn new_with_identity(
        mut config: EvpTransportConfig,
        producer: EvpProducerIdentity,
    ) -> Result<Self, String> {
        config.validate()?;

        // A local receiver must never receive direct-intake credentials, even
        // if a caller accidentally copied them onto the Agent endpoint.
        config.agent_endpoint.api_key = None;

        // Agent-only consumers never retain direct credentials.
        if config.mode == EvpTransportMode::AgentOnly {
            config.direct_endpoint = None;
        }

        let state = match config.mode {
            EvpTransportMode::AgentOnly => RouteState::Local(ProxyVersion::V2),
            EvpTransportMode::PreferLocalThenDirect => RouteState::Unresolved,
        };

        Ok(Self {
            id: NEXT_TRANSPORT_ID.fetch_add(1, Ordering::Relaxed),
            config: Arc::new(config),
            producer: Arc::new(producer),
            state: Arc::new(AsyncMutex::new(state)),
            clock: Arc::new(Instant::now),
            unavailable_recovery_cooldown: DEFAULT_UNAVAILABLE_RECOVERY_COOLDOWN,
        })
    }

    pub(crate) fn agent_only(
        endpoint: Endpoint,
        intake_subdomain: impl Into<String>,
    ) -> Result<Self, String> {
        Self::new_with_identity(
            EvpTransportConfig::agent_only(endpoint, intake_subdomain),
            EvpProducerIdentity::legacy_sidecar(),
        )
    }

    #[cfg(test)]
    pub(crate) fn shares_route_state(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    pub(crate) fn producer(&self) -> &EvpProducerIdentity {
        &self.producer
    }

    /// Compare effective configuration, not route state or instance identity.
    /// Reinstalling an unchanged session configuration must retain sticky
    /// routing, recovery deadlines, and the exposure deduplication scope.
    pub(crate) fn has_same_configuration(&self, other: &Self) -> bool {
        self.config == other.config && self.producer == other.producer
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

    /// Send one already-encoded payload through the selected route.
    ///
    /// A local 404/405 or a definitive pre-send failure replays the current
    /// payload directly. Ambiguous failures and local 403/429/5xx responses
    /// switch only future payloads, or enter cooldown when direct delivery is
    /// unavailable.
    pub(crate) async fn send_payload<C: HttpClientCapability + SleepCapability>(
        &self,
        client: &C,
        intake_path: &str,
        content_type: &str,
        payload: Bytes,
        log_prefix: &str,
        success_name: &str,
    ) -> bool {
        let request = EvpRequest {
            intake_path,
            content_type,
            payload,
            log_prefix,
            success_name,
        };
        let Some(route) = self.resolve_route(client, log_prefix).await else {
            debug!("{log_prefix}: no compatible EVP route is available");
            return false;
        };

        let first = self.send_once(client, route, &request).await;
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
        let switch_future = replay
            || matches!(&failure, DeliveryFailure::Ambiguous(_))
            || matches!(
                &failure,
                DeliveryFailure::Status(status)
                    if *status == 403 || *status == 429 || (500..600).contains(status)
            );

        if switch_future && self.leave_local_route().await && replay {
            let direct = self.send_once(client, Route::Direct, &request).await;
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
        log_prefix: &str,
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
        log_prefix: &str,
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
        if self.config.mode != EvpTransportMode::PreferLocalThenDirect {
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
        event: &EvpRequest<'_>,
    ) -> Result<(), DeliveryFailure> {
        let endpoint = match route {
            Route::Local(version) => agent_endpoint_with_path(
                &self.config.agent_endpoint,
                &join_paths(version.path(), event.intake_path),
            ),
            Route::Direct => self
                .config
                .direct_endpoint
                .as_ref()
                .ok_or_else(|| "direct endpoint is not configured".to_owned())
                .and_then(|base| endpoint_with_path(base, event.intake_path)),
        }
        .map_err(DeliveryFailure::DefinitivePreSend)?;

        let mut builder = endpoint
            .to_request_builder(USER_AGENT)
            .map_err(|error| DeliveryFailure::DefinitivePreSend(error.to_string()))?
            .method(Method::POST)
            .header("Content-Type", event.content_type)
            .header(EVP_ORIGIN_HEADER, self.producer.origin())
            .header(EVP_ORIGIN_VERSION_HEADER, self.producer.version());
        if matches!(route, Route::Local(_)) {
            builder = builder.header(EVP_SUBDOMAIN_HEADER, &self.config.intake_subdomain);
        }
        let request = builder
            .body(event.payload.clone())
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

        debug!(
            "{}: sent {}, status={status}",
            event.log_prefix, event.success_name
        );
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

fn log_failure(log_prefix: &str, failure: &DeliveryFailure) {
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
        info_release: Option<Arc<tokio::sync::Notify>>,
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
                info_release: None,
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
            let info_release = self.info_release.clone();
            async move {
                let is_info = request.uri().path().ends_with(INFO_PATH);
                let response = {
                    let mut script = inner.lock().unwrap();
                    script.requests.push(request);
                    script
                        .responses
                        .pop_front()
                        .expect("missing scripted HTTP response")
                };
                if is_info {
                    if let Some(release) = info_release {
                        release.notified().await;
                    }
                }
                response
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

    fn agentless(direct_key: Option<&'static str>) -> EvpTransport {
        EvpTransport::new_with_identity(
            EvpTransportConfig::prefer_local_then_direct(
                endpoint(
                    "http://agent.internal:8126/v0.4/traces",
                    Some("must-not-leak"),
                ),
                direct_key
                    .map(|key| endpoint("https://event-platform-intake.datadoghq.com/", Some(key))),
                evp_proxy::EVENT_PLATFORM_INTAKE_SUBDOMAIN,
            ),
            EvpProducerIdentity::new("dd-trace-rb", "3.0.0").unwrap(),
        )
        .unwrap()
    }

    async fn send(
        transport: &EvpTransport,
        client: &ScriptedCapabilities,
        path: &'static str,
    ) -> bool {
        transport
            .send_payload(
                client,
                path,
                "application/json",
                Bytes::from_static(b"{}"),
                "test",
                "batch",
            )
            .await
    }

    /// Poll both writers while the first discovery response is held. This
    /// deterministically exercises contention without sleeps or scheduler luck.
    async fn send_with_overlapping_discovery(
        transport: &EvpTransport,
        client: &mut ScriptedCapabilities,
        expected_probes: usize,
    ) {
        let release = Arc::new(tokio::sync::Notify::new());
        client.info_release = Some(release.clone());
        let other_writer = transport.clone();
        let exposure = send(transport, client, "/api/v2/exposures");
        let evaluation = send(&other_writer, client, "/api/v2/flagevaluation");
        tokio::pin!(exposure, evaluation);
        assert!(futures::poll!(&mut exposure).is_pending());
        assert!(futures::poll!(&mut evaluation).is_pending());
        assert_eq!(
            client
                .requests()
                .iter()
                .filter(|(url, _)| url.ends_with(INFO_PATH))
                .count(),
            expected_probes,
            "the second writer must wait for the in-flight discovery"
        );
        release.notify_one();
        let (exposure_sent, evaluation_sent) =
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(exposure, evaluation)
            })
            .await
            .expect("writers did not finish after releasing discovery");
        assert!(exposure_sent);
        assert!(evaluation_sent);
    }

    #[tokio::test]
    async fn concurrent_first_flush_prefers_v4_and_performs_one_shared_discovery() {
        let mut client = ScriptedCapabilities::new(vec![
            info_response(&["/evp_proxy/v2/", "/evp_proxy/v4"]),
            response(202, ""),
            response(202, ""),
        ]);
        let transport = agentless(Some("api-key"));
        let flag_transport = transport.clone();
        assert!(transport.shares_route_state(&flag_transport));

        send_with_overlapping_discovery(&transport, &mut client, 1).await;

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
        let transport = EvpTransport::new_with_identity(
            EvpTransportConfig::prefer_local_then_direct(
                endpoint(
                    "http://agent.internal:8126/customer/proxy/v0.4/traces",
                    None,
                ),
                Some(endpoint(
                    "https://event-platform-intake.datadoghq.com/",
                    Some("api-key"),
                )),
                EVP_SUBDOMAIN_VALUE,
            ),
            EvpProducerIdentity::new("dd-trace-rb", "3.0.0").unwrap(),
        )
        .unwrap();

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
    async fn generic_target_preserves_target_content_type_and_payload() {
        let local_client =
            ScriptedCapabilities::new(vec![info_response(&["/evp_proxy/v4"]), response(202, "")]);
        let local_transport = EvpTransport::new_with_identity(
            EvpTransportConfig::prefer_local_then_direct(
                endpoint("http://agent.internal:8126/", None),
                None,
                "errors-intake",
            ),
            EvpProducerIdentity::new("dd-trace-rb", "3.0.0").unwrap(),
        )
        .unwrap();
        assert!(
            local_transport
                .send_payload(
                    &local_client,
                    "/api/v2/logs",
                    "application/x-protobuf",
                    Bytes::from_static(b"local-payload"),
                    "test",
                    "generic batch",
                )
                .await
        );
        let local_requests = local_client.requests();
        assert_eq!(
            local_requests[1].1.get(EVP_SUBDOMAIN_HEADER).unwrap(),
            "errors-intake"
        );

        let client = ScriptedCapabilities::new(vec![info_response(&[]), response(202, "")]);
        let transport = EvpTransport::new_with_identity(
            EvpTransportConfig::prefer_local_then_direct(
                endpoint("http://agent.internal:8126/", None),
                Some(endpoint(
                    "https://errors-intake.datadoghq.com/",
                    Some("errors-key"),
                )),
                "errors-intake",
            ),
            EvpProducerIdentity::new("dd-trace-rb", "3.0.0").unwrap(),
        )
        .unwrap();
        let payload = Bytes::from_static(b"generic-evp-payload");

        assert!(
            transport
                .send_payload(
                    &client,
                    "/api/v2/logs",
                    "application/x-protobuf",
                    payload.clone(),
                    "test",
                    "generic batch",
                )
                .await
        );

        let script = client.inner.lock().unwrap();
        let request = &script.requests[1];
        assert_eq!(
            request.uri(),
            "https://errors-intake.datadoghq.com/api/v2/logs"
        );
        assert_eq!(
            request.headers().get("content-type").unwrap(),
            "application/x-protobuf"
        );
        assert_eq!(request.headers().get("dd-api-key").unwrap(), "errors-key");
        assert!(!request.headers().contains_key(EVP_SUBDOMAIN_HEADER));
        assert_eq!(request.body(), &payload);
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
    async fn non_replayable_local_statuses_switch_only_future_batches_to_direct() {
        for status in [403, 429, 500, 503] {
            let client = ScriptedCapabilities::new(vec![
                info_response(&["/evp_proxy/v4"]),
                response(status, "local rejection"),
                response(202, ""),
            ]);
            let transport = agentless(Some("api-key"));

            assert!(!send(&transport, &client, "/api/v2/exposures").await);
            assert_eq!(
                client.requests().len(),
                2,
                "status {status} replayed the current batch"
            );
            assert!(send(&transport, &client, "/api/v2/flagevaluation").await);

            let requests = client.requests();
            assert!(requests[1].0.contains("/evp_proxy/v4/"));
            assert!(
                requests[2].0.starts_with("https://event-platform-intake."),
                "status {status} did not move the future batch to direct intake"
            );
        }
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
    async fn non_replayable_local_statuses_enter_bounded_cooldown_without_direct_credentials() {
        for status in [403, 429, 500, 503] {
            let client = ScriptedCapabilities::new(vec![
                info_response(&["/evp_proxy/v4"]),
                response(status, "local rejection"),
                info_response(&["/evp_proxy/v2"]),
                response(202, ""),
            ]);
            let current = Arc::new(Mutex::new(Instant::now()));
            let clock_state = current.clone();
            let cooldown = Duration::from_secs(30);
            let transport = agentless(None)
                .with_clock(Arc::new(move || *clock_state.lock().unwrap()), cooldown);

            assert!(!send(&transport, &client, "/api/v2/exposures").await);
            assert_eq!(
                client.requests().len(),
                2,
                "status {status} replayed the current batch"
            );
            assert!(!send(&transport, &client, "/api/v2/flagevaluation").await);
            assert_eq!(
                client.requests().len(),
                2,
                "status {status} re-probed before the cooldown elapsed"
            );

            {
                let mut now = current.lock().unwrap();
                *now += cooldown;
            }
            assert!(send(&transport, &client, "/api/v2/flagevaluation").await);
            let requests = client.requests();
            assert_eq!(requests[2].0, "http://agent.internal:8126/info");
            assert!(requests[3].0.contains("/evp_proxy/v2/"));
        }
    }

    #[tokio::test]
    async fn unavailable_route_reprobes_once_after_cooldown_and_recovers() {
        let mut client = ScriptedCapabilities::new(vec![
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
        send_with_overlapping_discovery(&transport, &mut client, 2).await;

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
            let transport = EvpTransport::agent_only(
                endpoint(
                    "http://agent.internal:8126/v0.4/traces",
                    Some("must-not-leak"),
                ),
                EVP_SUBDOMAIN_VALUE,
            )
            .unwrap();

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

    #[test]
    fn agent_only_mode_discards_all_direct_credentials() {
        let transport = EvpTransport::new_with_identity(
            EvpTransportConfig {
                mode: EvpTransportMode::AgentOnly,
                agent_endpoint: endpoint(
                    "http://agent.internal:8126/v0.4/traces",
                    Some("agent-key-must-not-be-retained"),
                ),
                direct_endpoint: Some(endpoint(
                    "https://event-platform-intake.datadoghq.com/",
                    Some("direct-key-must-not-be-retained"),
                )),
                intake_subdomain: EVP_SUBDOMAIN_VALUE.to_owned(),
            },
            EvpProducerIdentity::new("dd-trace-rb", "3.0.0").unwrap(),
        )
        .unwrap();

        assert!(transport.config.agent_endpoint.api_key.is_none());
        assert!(transport.config.direct_endpoint.is_none());
    }

    #[tokio::test]
    async fn agent_only_v2_delivery_preserves_agent_path_prefix() {
        let client = ScriptedCapabilities::new(vec![response(202, "")]);
        let transport = EvpTransport::agent_only(
            endpoint(
                "http://agent.internal:8126/customer/proxy/v0.4/traces",
                None,
            ),
            EVP_SUBDOMAIN_VALUE,
        )
        .unwrap();

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

    #[tokio::test]
    async fn unchanged_session_configuration_preserves_sticky_direct_and_deduplication() {
        let session = crate::service::session_info::SessionInfo::default();
        let template = agentless(Some("api-key"));
        let config = EvpTransportConfigWithIdentity::new(
            (*template.config).clone(),
            (*template.producer).clone(),
        )
        .unwrap();
        session.set_evp_transport(config.clone()).unwrap();
        let before = session.get_evp_transport(EVP_SUBDOMAIN_VALUE).unwrap();
        let client = ScriptedCapabilities::new(vec![
            info_response(&[]),
            response(202, ""),
            response(202, ""),
        ]);
        assert!(send(&before, &client, "/api/v2/exposures").await);

        session.set_evp_transport(config).unwrap();
        let after = session.get_evp_transport(EVP_SUBDOMAIN_VALUE).unwrap();
        assert!(before.shares_route_state(&after));
        assert_eq!(before.deduplication_scope(), after.deduplication_scope());
        assert!(send(&after, &client, "/api/v2/flagevaluation").await);
        assert_eq!(client.requests().len(), 3, "configuration re-probed local");
    }

    #[tokio::test]
    async fn unchanged_session_configuration_preserves_unavailable_cooldown() {
        let session = crate::service::session_info::SessionInfo::default();
        let template = agentless(None);
        let config = EvpTransportConfigWithIdentity::new(
            (*template.config).clone(),
            (*template.producer).clone(),
        )
        .unwrap();
        session.set_evp_transport(config.clone()).unwrap();
        let before = session.get_evp_transport(EVP_SUBDOMAIN_VALUE).unwrap();
        let client = ScriptedCapabilities::new(vec![info_response(&[])]);
        assert!(!send(&before, &client, "/api/v2/exposures").await);
        session.set_evp_transport(config).unwrap();
        let after = session.get_evp_transport(EVP_SUBDOMAIN_VALUE).unwrap();
        assert!(before.shares_route_state(&after));
        assert!(!send(&after, &client, "/api/v2/flagevaluation").await);
        assert_eq!(
            client.requests().len(),
            1,
            "configuration bypassed cooldown"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn native_local_delivery_does_not_follow_redirects_or_retry_responses() {
        use httpmock::MockServer;
        use libdd_capabilities_impl::NativeCapabilities;

        for status in [202, 302, 403, 404, 405, 429, 500, 503] {
            let server = MockServer::start_async().await;
            let redirect = MockServer::start_async().await;
            let redirect_mock = redirect
                .mock_async(|_when, then| {
                    then.status(202);
                })
                .await;
            let info = server
                .mock_async(|when, then| {
                    when.method(httpmock::Method::GET).path("/prefix/info");
                    then.status(200).json_body(serde_json::json!({
                        "endpoints": ["/evp_proxy/v2", "/evp_proxy/v4"],
                        "evp_proxy_allowed_headers": [EVP_ORIGIN_HEADER, EVP_ORIGIN_VERSION_HEADER],
                    }));
                })
                .await;
            let delivery = server
                .mock_async(|when, then| {
                    when.method(httpmock::Method::POST)
                        .path("/prefix/evp_proxy/v4/api/v2/exposures")
                        .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
                        .header(EVP_ORIGIN_HEADER, "dd-trace-rb")
                        .header(EVP_ORIGIN_VERSION_HEADER, "3.0.0")
                        .header_missing("DD-API-KEY")
                        .body("{}");
                    then.status(status)
                        .header("Location", redirect.url("/redirect"));
                })
                .await;
            let transport = EvpTransport::new_with_identity(
                EvpTransportConfig::prefer_local_then_direct(
                    endpoint(&server.url("/prefix/v0.4/traces"), Some("must-not-leak")),
                    None,
                    EVP_SUBDOMAIN_VALUE,
                ),
                EvpProducerIdentity::new("dd-trace-rb", "3.0.0").unwrap(),
            )
            .unwrap();
            let client = NativeCapabilities::new_client();
            let sent = transport
                .send_payload(
                    &client,
                    "/api/v2/exposures",
                    "application/json",
                    Bytes::from_static(b"{}"),
                    "test",
                    "batch",
                )
                .await;
            assert_eq!(sent, status == 202);
            info.assert_calls_async(1).await;
            delivery.assert_calls_async(1).await;
            redirect_mock.assert_calls_async(0).await;
        }
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
            let config = EvpTransportConfig::prefer_local_then_direct(
                endpoint("http://agent.internal:8126/", None),
                Some(endpoint(url, Some("secret"))),
                EVP_SUBDOMAIN_VALUE,
            );
            assert!(config.validate().is_err(), "accepted direct URL {url}");
        }

        assert!(EvpTransportConfig::prefer_local_then_direct(
            endpoint("http://agent.internal:8126/", None),
            Some(endpoint(
                "https://event-platform-intake.datadoghq.eu/",
                Some("secret")
            )),
            EVP_SUBDOMAIN_VALUE,
        )
        .validate()
        .is_ok());

        // Custom and test Datadog sites remain valid. The client derives this
        // authority from DD_SITE and deliberately supplies the credential for
        // that exact host; validation binds the target label without imposing
        // a production-domain allowlist.
        assert!(EvpTransportConfig::prefer_local_then_direct(
            endpoint("http://agent.internal:8126/", None),
            Some(endpoint(
                "https://event-platform-intake.mock-intake.invalid/",
                Some("secret")
            )),
            EVP_SUBDOMAIN_VALUE,
        )
        .validate()
        .is_ok());

        assert!(EvpTransportConfig::prefer_local_then_direct(
            endpoint("http://agent.internal:8126/", None),
            Some(endpoint(
                "https://errors-intake.datadoghq.com/",
                Some("secret")
            )),
            "errors-intake",
        )
        .validate()
        .is_ok());

        assert!(EvpTransportConfig::prefer_local_then_direct(
            endpoint("http://agent.internal:8126/", None),
            Some(endpoint(
                "https://event-platform-intake.datadoghq.com/",
                Some("secret")
            )),
            "errors-intake",
        )
        .validate()
        .is_err());

        let longest_valid_site_for_target = [
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(39),
        ]
        .join(".");
        let valid_boundary_url =
            format!("https://{EVP_SUBDOMAIN_VALUE}.{longest_valid_site_for_target}/");
        assert_eq!(
            format!("{EVP_SUBDOMAIN_VALUE}.{longest_valid_site_for_target}").len(),
            MAX_DNS_HOST_LENGTH
        );
        assert!(EvpTransportConfig::prefer_local_then_direct(
            endpoint("http://agent.internal:8126/", None),
            Some(endpoint(&valid_boundary_url, Some("secret"))),
            EVP_SUBDOMAIN_VALUE,
        )
        .validate()
        .is_ok());

        let maximum_length_site = [
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(61),
        ]
        .join(".");
        assert_eq!(maximum_length_site.len(), MAX_DNS_HOST_LENGTH);
        let oversized_composed_host =
            format!("https://{EVP_SUBDOMAIN_VALUE}.{maximum_length_site}/");
        assert!(EvpTransportConfig::prefer_local_then_direct(
            endpoint("http://agent.internal:8126/", None),
            Some(endpoint(&oversized_composed_host, Some("secret"))),
            EVP_SUBDOMAIN_VALUE,
        )
        .validate()
        .is_err());

        for subdomain in [
            "",
            "Event-platform-intake",
            "event.platform.intake",
            "-event-platform-intake",
            "event-platform-intake-",
            "event_platform_intake",
        ] {
            let config = EvpTransportConfig::agent_only(
                endpoint("http://agent.internal:8126/", None),
                subdomain,
            );
            assert!(
                config.validate().is_err(),
                "accepted invalid intake subdomain {subdomain:?}"
            );
        }
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
        assert!(EvpProducerIdentity::new("", "1.0.0").is_err());
        assert!(EvpProducerIdentity::new("dd-trace-rb", "invalid\nversion").is_err());
        assert!(EvpProducerIdentity::new(
            "x".repeat(MAX_EVP_PRODUCER_IDENTITY_LENGTH + 1),
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
        assert!(bincode::deserialize::<EvpProducerIdentity>(&bytes).is_err());
    }
}
