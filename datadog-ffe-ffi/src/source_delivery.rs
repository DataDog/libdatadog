// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use datadog_ffe::rules_based::{
    now, Assignment, Configuration, EvaluationContext, EvaluationError, ExpectedFlagType,
    UniversalFlagConfig,
};
use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};

/// Explicit native-owned Feature Flags source delivery configuration.
#[derive(Clone)]
pub struct FfeSourceDeliveryConfig {
    /// Fully qualified CDN UFC endpoint URL.
    pub base_url: String,
    /// Optional API key. The key is used for request headers and redacted from debug output.
    pub api_key: Option<String>,
    /// Poll interval for language runtimes that choose to schedule repeated polls.
    pub poll_interval: Duration,
    /// Per-request network timeout.
    pub request_timeout: Duration,
    /// Maximum retries after the first request.
    pub max_retries: u32,
    /// Initial retry backoff.
    pub backoff_base: Duration,
}

impl fmt::Debug for FfeSourceDeliveryConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FfeSourceDeliveryConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("poll_interval", &self.poll_interval)
            .field("request_timeout", &self.request_timeout)
            .field("max_retries", &self.max_retries)
            .field("backoff_base", &self.backoff_base)
            .finish()
    }
}

/// Native source delivery lifecycle and poll status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfeSourceDeliveryStatus {
    /// A valid UFC payload was fetched and applied.
    Applied {
        /// HTTP status code.
        status_code: u16,
        /// Number of attempts used.
        attempts: u32,
        /// Accepted ETag, if any.
        etag: Option<String>,
    },
    /// The CDN reported that the payload did not change.
    Unchanged {
        /// HTTP status code.
        status_code: u16,
        /// Number of attempts used.
        attempts: u32,
    },
    /// A poll was skipped because another poll was already active.
    Skipped {
        /// Number of attempts used.
        attempts: u32,
    },
    /// Lifecycle was explicitly started.
    Started,
    /// Lifecycle was explicitly shut down.
    Shutdown,
}

impl FfeSourceDeliveryStatus {
    /// Stable status name for language wrappers.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Applied { .. } => "applied",
            Self::Unchanged { .. } => "unchanged",
            Self::Skipped { .. } => "skipped",
            Self::Started => "started",
            Self::Shutdown => "shutdown",
        }
    }

    /// HTTP status code, when the status came from an HTTP response.
    pub fn status_code(&self) -> Option<u16> {
        match self {
            Self::Applied { status_code, .. } | Self::Unchanged { status_code, .. } => {
                Some(*status_code)
            }
            Self::Skipped { .. } | Self::Started | Self::Shutdown => None,
        }
    }

    /// Number of attempts used by a poll status.
    pub fn attempts(&self) -> u32 {
        match self {
            Self::Applied { attempts, .. }
            | Self::Unchanged { attempts, .. }
            | Self::Skipped { attempts } => *attempts,
            Self::Started | Self::Shutdown => 0,
        }
    }

    /// True when a valid UFC payload was applied.
    pub fn applied(&self) -> bool {
        matches!(self, Self::Applied { .. })
    }

    /// True when the CDN returned an unchanged response.
    pub fn unchanged(&self) -> bool {
        matches!(self, Self::Unchanged { .. })
    }

    /// True when a poll was skipped due to no-overlap protection.
    pub fn skipped(&self) -> bool {
        matches!(self, Self::Skipped { .. })
    }

    /// Accepted ETag for applied statuses.
    pub fn etag(&self) -> Option<&str> {
        match self {
            Self::Applied { etag, .. } => etag.as_deref(),
            _ => None,
        }
    }
}

/// Source delivery error category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfeSourceDeliveryErrorKind {
    /// Configuration was invalid.
    InvalidConfig,
    /// Network or HTTP transport failed.
    Transport,
    /// CDN returned a non-success HTTP status.
    HttpStatus,
    /// UFC payload parsing failed.
    Parse,
    /// Internal state lock was poisoned.
    State,
}

/// Bounded native source delivery error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfeSourceDeliveryError {
    kind: FfeSourceDeliveryErrorKind,
    status_code: Option<u16>,
    retryable: bool,
    message: String,
}

impl FfeSourceDeliveryError {
    /// Invalid configuration.
    pub fn invalid_config(message: impl Into<String>) -> Self {
        Self::new(
            FfeSourceDeliveryErrorKind::InvalidConfig,
            None,
            false,
            message,
        )
    }

    /// Transport failure.
    pub fn transport(message: impl Into<String>) -> Self {
        Self::new(FfeSourceDeliveryErrorKind::Transport, None, false, message)
    }

    /// HTTP status failure.
    pub fn http_status(status_code: u16, retryable: bool) -> Self {
        Self::new(
            FfeSourceDeliveryErrorKind::HttpStatus,
            Some(status_code),
            retryable,
            format!("feature flag CDN request failed with status {status_code}"),
        )
    }

    fn parse(message: impl Into<String>) -> Self {
        Self::new(FfeSourceDeliveryErrorKind::Parse, None, false, message)
    }

    fn state(message: impl Into<String>) -> Self {
        Self::new(FfeSourceDeliveryErrorKind::State, None, false, message)
    }

    fn new(
        kind: FfeSourceDeliveryErrorKind,
        status_code: Option<u16>,
        retryable: bool,
        message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            status_code,
            retryable,
            message: message.into(),
        }
    }

    /// Error category.
    pub fn kind(&self) -> FfeSourceDeliveryErrorKind {
        self.kind
    }

    /// HTTP status code, if available.
    pub fn status_code(&self) -> Option<u16> {
        self.status_code
    }

    /// True when retrying this error is allowed by CDN semantics.
    pub fn retryable(&self) -> bool {
        self.retryable
    }
}

impl fmt::Display for FfeSourceDeliveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FfeSourceDeliveryError {}

/// HTTP request passed to the native delivery transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfeSourceDeliveryRequest {
    url: String,
    headers: Vec<(String, String)>,
    timeout: Duration,
}

impl FfeSourceDeliveryRequest {
    fn new(url: String, timeout: Duration) -> Self {
        Self {
            url,
            headers: Vec::new(),
            timeout,
        }
    }

    fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Request URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Request timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Header lookup used by tests and language wrappers.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// HTTP response returned by the native delivery transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfeSourceDeliveryResponse {
    /// HTTP status code.
    pub status_code: u16,
    /// Response headers.
    pub headers: Vec<(String, String)>,
    /// Response body bytes.
    pub body: Vec<u8>,
}

trait SourceDeliveryTransport: Send + Sync {
    fn send(
        &self,
        request: FfeSourceDeliveryRequest,
    ) -> Result<FfeSourceDeliveryResponse, FfeSourceDeliveryError>;
}

#[derive(Debug)]
struct LibddHttpTransport;

impl SourceDeliveryTransport for LibddHttpTransport {
    fn send(
        &self,
        request: FfeSourceDeliveryRequest,
    ) -> Result<FfeSourceDeliveryResponse, FfeSourceDeliveryError> {
        let client = HttpClient::builder()
            .base_url(request.url().to_string())
            .timeout(request.timeout())
            .treat_http_errors_as_errors(false)
            .build()
            .map_err(|err| FfeSourceDeliveryError::invalid_config(err.to_string()))?;

        let mut http_request = HttpRequest::new(HttpMethod::Get, request.url().to_string())
            .with_timeout(request.timeout());
        for (name, value) in request.headers {
            http_request = http_request.with_header(name, value);
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| FfeSourceDeliveryError::transport(err.to_string()))?;
        let response = runtime
            .block_on(client.send(http_request))
            .map_err(|err| FfeSourceDeliveryError::transport(err.to_string()))?;

        Ok(FfeSourceDeliveryResponse {
            status_code: response.status_code(),
            headers: response.headers().to_vec(),
            body: response.body().to_vec(),
        })
    }
}

#[derive(Debug)]
struct SourceDeliveryState {
    started: bool,
    shutdown: bool,
    last_etag: Option<String>,
    configuration: Option<Configuration>,
}

/// Native-owned source delivery handle.
pub struct FfeSourceDeliveryHandle {
    config: FfeSourceDeliveryConfig,
    transport: Box<dyn SourceDeliveryTransport>,
    state: Mutex<SourceDeliveryState>,
    in_flight: AtomicBool,
}

impl FfeSourceDeliveryHandle {
    /// Build a source delivery handle with the production HTTP transport.
    pub fn new(config: FfeSourceDeliveryConfig) -> Result<Self, FfeSourceDeliveryError> {
        validate_config(&config)?;
        Ok(Self::from_transport(config, Box::new(LibddHttpTransport)))
    }

    fn from_transport(
        config: FfeSourceDeliveryConfig,
        transport: Box<dyn SourceDeliveryTransport>,
    ) -> Self {
        Self {
            config,
            transport,
            state: Mutex::new(SourceDeliveryState {
                started: false,
                shutdown: false,
                last_etag: None,
                configuration: None,
            }),
            in_flight: AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    fn new_with_transport(
        config: FfeSourceDeliveryConfig,
        transport: impl SourceDeliveryTransport + 'static,
    ) -> Self {
        Self::from_transport(config, Box::new(transport))
    }

    /// Explicitly mark source delivery started. This POC does not spawn native worker threads.
    pub fn start(&self) -> Result<FfeSourceDeliveryStatus, FfeSourceDeliveryError> {
        let mut state = self.lock_state()?;
        state.started = true;
        state.shutdown = false;
        Ok(FfeSourceDeliveryStatus::Started)
    }

    /// Poll the configured CDN once.
    pub fn poll_once(&self) -> Result<FfeSourceDeliveryStatus, FfeSourceDeliveryError> {
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Ok(FfeSourceDeliveryStatus::Skipped { attempts: 0 });
        }

        let result = self.poll_once_inner();
        self.in_flight.store(false, Ordering::Release);
        result
    }

    fn poll_once_inner(&self) -> Result<FfeSourceDeliveryStatus, FfeSourceDeliveryError> {
        let mut attempts = 0;
        loop {
            attempts += 1;
            let request = self.build_request()?;
            let response = self.transport.send(request)?;
            match self.handle_response(response, attempts) {
                Ok(status) => return Ok(status),
                Err(err)
                    if err.retryable()
                        && attempts <= self.config.max_retries
                        && self.config.backoff_base > Duration::ZERO =>
                {
                    std::thread::sleep(backoff_for_attempt(self.config.backoff_base, attempts));
                }
                Err(err) if err.retryable() && attempts <= self.config.max_retries => {}
                Err(err) => return Err(err),
            }
        }
    }

    fn build_request(&self) -> Result<FfeSourceDeliveryRequest, FfeSourceDeliveryError> {
        let last_etag = self.last_etag();
        let mut request = FfeSourceDeliveryRequest::new(
            self.config.base_url.clone(),
            self.config.request_timeout,
        );
        if let Some(api_key) = self.config.api_key.as_ref() {
            request = request.with_header("DD-API-KEY", api_key);
        }
        if let Some(etag) = last_etag {
            request = request.with_header("If-None-Match", etag);
        }
        Ok(request)
    }

    fn handle_response(
        &self,
        response: FfeSourceDeliveryResponse,
        attempts: u32,
    ) -> Result<FfeSourceDeliveryStatus, FfeSourceDeliveryError> {
        if response.status_code == 304 {
            return Ok(FfeSourceDeliveryStatus::Unchanged {
                status_code: response.status_code,
                attempts,
            });
        }

        if response.status_code == 200 {
            let next_config = Configuration::from_server_response(
                UniversalFlagConfig::from_json(response.body).map_err(|err| {
                    FfeSourceDeliveryError::parse(format!(
                        "feature flag CDN returned malformed UFC payload: {err}"
                    ))
                })?,
            );
            let next_etag = header_get(&response.headers, "ETag");
            let mut state = self.lock_state()?;
            state.configuration = Some(next_config);
            state.last_etag = next_etag.clone();
            return Ok(FfeSourceDeliveryStatus::Applied {
                status_code: response.status_code,
                attempts,
                etag: next_etag,
            });
        }

        Err(FfeSourceDeliveryError::http_status(
            response.status_code,
            is_retryable_status(response.status_code),
        ))
    }

    /// Evaluate a flag with the last accepted native configuration.
    pub fn resolve_value(
        &self,
        flag_key: &str,
        expected_type: ExpectedFlagType,
        context: &EvaluationContext,
    ) -> Result<Result<Assignment, EvaluationError>, FfeSourceDeliveryError> {
        let state = self.lock_state()?;
        let Some(configuration) = state.configuration.as_ref() else {
            return Ok(Err(EvaluationError::ConfigurationMissing));
        };
        Ok(configuration.eval_flag(flag_key, context, expected_type, now()))
    }

    /// Explicitly shut down source delivery.
    pub fn shutdown(
        &self,
        _timeout: Duration,
    ) -> Result<FfeSourceDeliveryStatus, FfeSourceDeliveryError> {
        let mut state = self.lock_state()?;
        state.started = false;
        state.shutdown = true;
        Ok(FfeSourceDeliveryStatus::Shutdown)
    }

    /// True when a valid UFC payload has been applied.
    pub fn is_ready(&self) -> bool {
        match self.state.lock() {
            Ok(state) => state.configuration.is_some(),
            Err(_) => false,
        }
    }

    /// True when source delivery has been explicitly started.
    pub fn is_started(&self) -> bool {
        match self.state.lock() {
            Ok(state) => state.started,
            Err(_) => false,
        }
    }

    /// Last accepted ETag.
    pub fn last_etag(&self) -> Option<String> {
        match self.state.lock() {
            Ok(state) => state.last_etag.clone(),
            Err(_) => None,
        }
    }

    fn lock_state(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, SourceDeliveryState>, FfeSourceDeliveryError> {
        self.state
            .lock()
            .map_err(|_| FfeSourceDeliveryError::state("source delivery state lock poisoned"))
    }

    #[cfg(test)]
    fn try_begin_poll_for_test(&self) -> Result<(), FfeSourceDeliveryError> {
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(FfeSourceDeliveryError::state("poll already active"));
        }
        Ok(())
    }

    #[cfg(test)]
    fn end_poll_for_test(&self) {
        self.in_flight.store(false, Ordering::Release);
    }
}

fn validate_config(config: &FfeSourceDeliveryConfig) -> Result<(), FfeSourceDeliveryError> {
    if config.base_url.trim().is_empty() {
        return Err(FfeSourceDeliveryError::invalid_config(
            "feature flag CDN base URL must not be empty",
        ));
    }
    if config.request_timeout == Duration::ZERO {
        return Err(FfeSourceDeliveryError::invalid_config(
            "feature flag CDN request timeout must be positive",
        ));
    }
    if config.poll_interval == Duration::ZERO {
        return Err(FfeSourceDeliveryError::invalid_config(
            "feature flag CDN poll interval must be positive",
        ));
    }
    Ok(())
}

fn header_get(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

fn is_retryable_status(status_code: u16) -> bool {
    status_code == 429 || status_code >= 500
}

fn backoff_for_attempt(base: Duration, attempt: u32) -> Duration {
    base.saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::{
        FfeSourceDeliveryConfig, FfeSourceDeliveryError, FfeSourceDeliveryHandle,
        FfeSourceDeliveryRequest, FfeSourceDeliveryResponse, FfeSourceDeliveryStatus,
        SourceDeliveryTransport,
    };

    #[derive(Clone)]
    struct FakeTransport {
        responses: Arc<Mutex<Vec<Result<FfeSourceDeliveryResponse, FfeSourceDeliveryError>>>>,
        requests: Arc<Mutex<Vec<FfeSourceDeliveryRequest>>>,
    }

    impl FakeTransport {
        fn new(responses: Vec<Result<FfeSourceDeliveryResponse, FfeSourceDeliveryError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses)),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn requests(&self) -> Vec<FfeSourceDeliveryRequest> {
            match self.requests.lock() {
                Ok(requests) => requests.clone(),
                Err(_) => Vec::new(),
            }
        }
    }

    impl SourceDeliveryTransport for FakeTransport {
        fn send(
            &self,
            request: FfeSourceDeliveryRequest,
        ) -> Result<FfeSourceDeliveryResponse, FfeSourceDeliveryError> {
            if let Ok(mut requests) = self.requests.lock() {
                requests.push(request);
            }
            match self.responses.lock() {
                Ok(mut responses) if !responses.is_empty() => responses.remove(0),
                _ => Err(FfeSourceDeliveryError::transport(
                    "fake transport exhausted",
                )),
            }
        }
    }

    fn explicit_config() -> FfeSourceDeliveryConfig {
        FfeSourceDeliveryConfig {
            base_url: "http://127.0.0.1:8123/mock/ufc/config".to_string(),
            api_key: Some("explicit-test-key".to_string()),
            poll_interval: Duration::from_secs(60),
            request_timeout: Duration::from_secs(1),
            max_retries: 0,
            backoff_base: Duration::ZERO,
        }
    }

    fn valid_control_bytes() -> Vec<u8> {
        br#"{
            "id": "native-source-test-config",
            "createdAt": "2025-10-31T00:00:00Z",
            "format": "SERVER",
            "environment": { "name": "test" },
            "flags": {
                "valid-control": {
                    "key": "valid-control",
                    "enabled": true,
                    "variationType": "BOOLEAN",
                    "variations": {
                        "true": { "key": "true", "value": true },
                        "false": { "key": "false", "value": false }
                    },
                    "allocations": [
                        {
                            "key": "allocation-default",
                            "splits": [
                                { "variationKey": "true", "shards": [] }
                            ],
                            "doLog": true
                        }
                    ]
                }
            }
        }"#
        .to_vec()
    }

    #[test]
    fn explicit_config_is_required_and_debug_redacts_secrets() {
        std::env::set_var("DD_FFE_TEST_HIDDEN_KEY", "hidden-env-key");
        let transport = FakeTransport::new(vec![Ok(FfeSourceDeliveryResponse {
            status_code: 200,
            headers: vec![("ETag".to_string(), "\"ufc-v1\"".to_string())],
            body: valid_control_bytes(),
        })]);
        let handle =
            FfeSourceDeliveryHandle::new_with_transport(explicit_config(), transport.clone());

        let status = handle.poll_once();

        assert!(matches!(
            status,
            Ok(FfeSourceDeliveryStatus::Applied { .. })
        ));
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].header("DD-API-KEY"), Some("explicit-test-key"));
        assert_ne!(requests[0].header("DD-API-KEY"), Some("hidden-env-key"));
        assert!(!format!("{:?}", explicit_config()).contains("explicit-test-key"));
        assert!(!FfeSourceDeliveryError::transport("failed")
            .to_string()
            .contains("explicit-test-key"));
    }

    #[test]
    fn poll_once_applies_only_valid_payloads_preserves_lkg_and_uses_etag() {
        let transport = FakeTransport::new(vec![
            Ok(FfeSourceDeliveryResponse {
                status_code: 200,
                headers: vec![("ETag".to_string(), "\"ufc-v1\"".to_string())],
                body: valid_control_bytes(),
            }),
            Ok(FfeSourceDeliveryResponse {
                status_code: 304,
                headers: Vec::new(),
                body: Vec::new(),
            }),
            Ok(FfeSourceDeliveryResponse {
                status_code: 200,
                headers: vec![("ETag".to_string(), "\"bad\"".to_string())],
                body: br#"{"flags":"#.to_vec(),
            }),
        ]);
        let handle =
            FfeSourceDeliveryHandle::new_with_transport(explicit_config(), transport.clone());

        let first = handle.poll_once();
        let second = handle.poll_once();
        let malformed_warm = handle.poll_once();

        assert!(matches!(
            first,
            Ok(FfeSourceDeliveryStatus::Applied {
                status_code: 200,
                ..
            })
        ));
        assert!(matches!(
            second,
            Ok(FfeSourceDeliveryStatus::Unchanged {
                status_code: 304,
                ..
            })
        ));
        assert!(matches!(malformed_warm, Err(_)));
        assert!(handle.is_ready());
        assert_eq!(handle.last_etag().as_deref(), Some("\"ufc-v1\""));

        let requests = transport.requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[1].header("If-None-Match"), Some("\"ufc-v1\""));
        assert_eq!(requests[2].header("If-None-Match"), Some("\"ufc-v1\""));
    }

    #[test]
    fn retry_policy_retries_only_429_and_5xx() {
        let mut config = explicit_config();
        config.max_retries = 2;
        let retrying = FakeTransport::new(vec![
            Ok(FfeSourceDeliveryResponse {
                status_code: 500,
                headers: Vec::new(),
                body: Vec::new(),
            }),
            Ok(FfeSourceDeliveryResponse {
                status_code: 429,
                headers: Vec::new(),
                body: Vec::new(),
            }),
            Ok(FfeSourceDeliveryResponse {
                status_code: 200,
                headers: vec![("ETag".to_string(), "\"ufc-v1\"".to_string())],
                body: valid_control_bytes(),
            }),
        ]);
        let handle = FfeSourceDeliveryHandle::new_with_transport(config, retrying.clone());

        let result = handle.poll_once();

        assert!(matches!(
            result,
            Ok(FfeSourceDeliveryStatus::Applied { attempts: 3, .. })
        ));
        assert_eq!(retrying.requests().len(), 3);

        let non_retrying = FakeTransport::new(vec![Ok(FfeSourceDeliveryResponse {
            status_code: 401,
            headers: Vec::new(),
            body: Vec::new(),
        })]);
        let handle =
            FfeSourceDeliveryHandle::new_with_transport(explicit_config(), non_retrying.clone());

        let result = handle.poll_once();

        assert!(matches!(result, Err(_)));
        assert_eq!(non_retrying.requests().len(), 1);
    }

    #[test]
    fn lifecycle_shutdown_and_no_overlap_are_explicit_and_bounded() {
        let handle = Arc::new(FfeSourceDeliveryHandle::new_with_transport(
            explicit_config(),
            FakeTransport::new(vec![Ok(FfeSourceDeliveryResponse {
                status_code: 200,
                headers: Vec::new(),
                body: valid_control_bytes(),
            })]),
        ));

        assert!(!handle.is_started());
        assert!(matches!(
            handle.start(),
            Ok(FfeSourceDeliveryStatus::Started)
        ));
        assert!(handle.is_started());
        assert!(matches!(handle.try_begin_poll_for_test(), Ok(())));
        assert!(matches!(
            handle.poll_once(),
            Ok(FfeSourceDeliveryStatus::Skipped { .. })
        ));
        handle.end_poll_for_test();
        assert!(matches!(
            handle.shutdown(Duration::from_millis(1)),
            Ok(FfeSourceDeliveryStatus::Shutdown)
        ));
        assert!(!handle.is_started());
    }
}
