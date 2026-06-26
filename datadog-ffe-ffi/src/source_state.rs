// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::fmt;
use std::sync::Mutex;
use std::time::Duration;

use datadog_ffe::rules_based::{
    now, Assignment, Configuration, EvaluationContext, EvaluationError, ExpectedFlagType,
    UniversalFlagConfig,
};
use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};

/// Explicit Feature Flags source-state configuration for caller-driven polling.
#[derive(Clone)]
pub struct FfeSourceStateConfig {
    /// Fully qualified CDN UFC endpoint URL.
    pub base_url: String,
    /// Optional API key. The key is used for request headers and redacted from debug output.
    pub api_key: Option<String>,
    /// Per-request network timeout.
    pub request_timeout: Duration,
    /// Maximum retries after the first request.
    pub max_retries: u32,
    /// Initial retry backoff for retryable CDN statuses.
    pub backoff_base: Duration,
}

impl fmt::Debug for FfeSourceStateConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FfeSourceStateConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("request_timeout", &self.request_timeout)
            .field("max_retries", &self.max_retries)
            .field("backoff_base", &self.backoff_base)
            .finish()
    }
}

/// Caller-driven source-state poll outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfeSourcePollOutcome {
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
}

impl FfeSourcePollOutcome {
    /// Stable status name for language wrappers.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Applied { .. } => "applied",
            Self::Unchanged { .. } => "unchanged",
        }
    }

    /// HTTP status code.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::Applied { status_code, .. } | Self::Unchanged { status_code, .. } => *status_code,
        }
    }

    /// Number of attempts used by a poll outcome.
    pub fn attempts(&self) -> u32 {
        match self {
            Self::Applied { attempts, .. } | Self::Unchanged { attempts, .. } => *attempts,
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

    /// Accepted ETag for applied statuses.
    pub fn etag(&self) -> Option<&str> {
        match self {
            Self::Applied { etag, .. } => etag.as_deref(),
            Self::Unchanged { .. } => None,
        }
    }
}

/// Source-state error category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfeSourceApplyErrorKind {
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

/// Bounded source-state fetch/apply error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfeSourceApplyError {
    kind: FfeSourceApplyErrorKind,
    status_code: Option<u16>,
    retryable: bool,
    message: String,
}

impl FfeSourceApplyError {
    /// Invalid configuration.
    pub fn invalid_config(message: impl Into<String>) -> Self {
        Self::new(FfeSourceApplyErrorKind::InvalidConfig, None, false, message)
    }

    /// Transport failure.
    pub fn transport(message: impl Into<String>) -> Self {
        Self::new(FfeSourceApplyErrorKind::Transport, None, false, message)
    }

    /// HTTP status failure.
    pub fn http_status(status_code: u16, retryable: bool) -> Self {
        Self::new(
            FfeSourceApplyErrorKind::HttpStatus,
            Some(status_code),
            retryable,
            format!("feature flag CDN request failed with status {status_code}"),
        )
    }

    fn parse(message: impl Into<String>) -> Self {
        Self::new(FfeSourceApplyErrorKind::Parse, None, false, message)
    }

    fn state(message: impl Into<String>) -> Self {
        Self::new(FfeSourceApplyErrorKind::State, None, false, message)
    }

    fn new(
        kind: FfeSourceApplyErrorKind,
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
    pub fn kind(&self) -> FfeSourceApplyErrorKind {
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

impl fmt::Display for FfeSourceApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FfeSourceApplyError {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FfeSourceRequest {
    url: String,
    headers: Vec<(String, String)>,
    timeout: Duration,
}

impl FfeSourceRequest {
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

    fn url(&self) -> &str {
        &self.url
    }

    #[cfg(test)]
    fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    fn timeout(&self) -> Duration {
        self.timeout
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FfeSourceResponse {
    status_code: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

trait SourceStateTransport: Send + Sync {
    fn send(&self, request: FfeSourceRequest) -> Result<FfeSourceResponse, FfeSourceApplyError>;
}

#[derive(Debug)]
struct LibddHttpTransport;

impl SourceStateTransport for LibddHttpTransport {
    fn send(&self, request: FfeSourceRequest) -> Result<FfeSourceResponse, FfeSourceApplyError> {
        let client = HttpClient::builder()
            .base_url(request.url().to_string())
            .timeout(request.timeout())
            .treat_http_errors_as_errors(false)
            .build()
            .map_err(|err| FfeSourceApplyError::invalid_config(err.to_string()))?;

        let mut http_request = HttpRequest::new(HttpMethod::Get, request.url().to_string())
            .with_timeout(request.timeout());
        for (name, value) in request.headers {
            http_request = http_request.with_header(name, value);
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| FfeSourceApplyError::transport(err.to_string()))?;
        let response = runtime
            .block_on(client.send(http_request))
            .map_err(|err| FfeSourceApplyError::transport(err.to_string()))?;

        Ok(FfeSourceResponse {
            status_code: response.status_code(),
            headers: response.headers().to_vec(),
            body: response.body().to_vec(),
        })
    }
}

#[derive(Debug)]
struct SourceStateInner {
    last_etag: Option<String>,
    configuration: Option<Configuration>,
}

/// Caller-driven native source state.
pub struct FfeSourceState {
    config: FfeSourceStateConfig,
    transport: Box<dyn SourceStateTransport>,
    state: Mutex<SourceStateInner>,
}

impl FfeSourceState {
    /// Build a source-state primitive with the production HTTP transport.
    pub fn new(config: FfeSourceStateConfig) -> Result<Self, FfeSourceApplyError> {
        validate_config(&config)?;
        Ok(Self::from_transport(config, Box::new(LibddHttpTransport)))
    }

    fn from_transport(
        config: FfeSourceStateConfig,
        transport: Box<dyn SourceStateTransport>,
    ) -> Self {
        Self {
            config,
            transport,
            state: Mutex::new(SourceStateInner {
                last_etag: None,
                configuration: None,
            }),
        }
    }

    #[cfg(test)]
    fn new_with_transport(
        config: FfeSourceStateConfig,
        transport: impl SourceStateTransport + 'static,
    ) -> Self {
        Self::from_transport(config, Box::new(transport))
    }

    /// Poll the configured CDN once. This primitive does not start or own a background worker.
    pub fn poll_once(&self) -> Result<FfeSourcePollOutcome, FfeSourceApplyError> {
        let mut attempts = 0;
        loop {
            attempts += 1;
            let request = self.build_request()?;
            let response = self.transport.send(request)?;
            match self.handle_response(response, attempts) {
                Ok(outcome) => return Ok(outcome),
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

    fn build_request(&self) -> Result<FfeSourceRequest, FfeSourceApplyError> {
        let mut request =
            FfeSourceRequest::new(self.config.base_url.clone(), self.config.request_timeout);
        if let Some(api_key) = self.config.api_key.as_ref() {
            request = request.with_header("DD-API-KEY", api_key);
        }
        if let Some(etag) = self.last_etag() {
            request = request.with_header("If-None-Match", etag);
        }
        Ok(request)
    }

    fn handle_response(
        &self,
        response: FfeSourceResponse,
        attempts: u32,
    ) -> Result<FfeSourcePollOutcome, FfeSourceApplyError> {
        if response.status_code == 304 {
            return Ok(FfeSourcePollOutcome::Unchanged {
                status_code: response.status_code,
                attempts,
            });
        }

        if response.status_code == 200 {
            let next_config = Configuration::from_server_response(
                UniversalFlagConfig::from_json(response.body).map_err(|err| {
                    FfeSourceApplyError::parse(format!(
                        "feature flag CDN returned malformed UFC payload: {err}"
                    ))
                })?,
            );
            let next_etag = header_get(&response.headers, "ETag");
            let mut state = self.lock_state()?;
            state.configuration = Some(next_config);
            state.last_etag = next_etag.clone();
            return Ok(FfeSourcePollOutcome::Applied {
                status_code: response.status_code,
                attempts,
                etag: next_etag,
            });
        }

        Err(FfeSourceApplyError::http_status(
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
    ) -> Result<Result<Assignment, EvaluationError>, FfeSourceApplyError> {
        let state = self.lock_state()?;
        let Some(configuration) = state.configuration.as_ref() else {
            return Ok(Err(EvaluationError::ConfigurationMissing));
        };
        Ok(configuration.eval_flag(flag_key, context, expected_type, now()))
    }

    /// True when a valid UFC payload has been applied.
    pub fn is_ready(&self) -> bool {
        match self.state.lock() {
            Ok(state) => state.configuration.is_some(),
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
    ) -> Result<std::sync::MutexGuard<'_, SourceStateInner>, FfeSourceApplyError> {
        self.state
            .lock()
            .map_err(|_| FfeSourceApplyError::state("source state lock poisoned"))
    }
}

fn validate_config(config: &FfeSourceStateConfig) -> Result<(), FfeSourceApplyError> {
    if config.base_url.trim().is_empty() {
        return Err(FfeSourceApplyError::invalid_config(
            "feature flag CDN base URL must not be empty",
        ));
    }
    if config.request_timeout == Duration::ZERO {
        return Err(FfeSourceApplyError::invalid_config(
            "feature flag CDN request timeout must be positive",
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
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use datadog_ffe::rules_based::{EvaluationContext, ExpectedFlagType, Str};

    use super::{
        FfeSourceApplyError, FfeSourcePollOutcome, FfeSourceResponse, FfeSourceState,
        FfeSourceStateConfig, SourceStateTransport,
    };

    #[derive(Clone)]
    struct FakeTransport {
        responses: Arc<Mutex<Vec<Result<FfeSourceResponse, FfeSourceApplyError>>>>,
        requests: Arc<Mutex<Vec<Vec<(String, String)>>>>,
    }

    impl FakeTransport {
        fn new(responses: Vec<Result<FfeSourceResponse, FfeSourceApplyError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses)),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn requests(&self) -> Vec<Vec<(String, String)>> {
            match self.requests.lock() {
                Ok(requests) => requests.clone(),
                Err(_) => Vec::new(),
            }
        }
    }

    impl SourceStateTransport for FakeTransport {
        fn send(
            &self,
            request: super::FfeSourceRequest,
        ) -> Result<FfeSourceResponse, FfeSourceApplyError> {
            if let Ok(mut requests) = self.requests.lock() {
                requests.push(request.headers().to_vec());
            }
            match self.responses.lock() {
                Ok(mut responses) if !responses.is_empty() => responses.remove(0),
                _ => Err(FfeSourceApplyError::transport("fake transport exhausted")),
            }
        }
    }

    fn explicit_config() -> FfeSourceStateConfig {
        FfeSourceStateConfig {
            base_url: "http://127.0.0.1:8123/mock/ufc/config".to_string(),
            api_key: Some("explicit-test-key".to_string()),
            request_timeout: Duration::from_secs(1),
            max_retries: 0,
            backoff_base: Duration::ZERO,
        }
    }

    fn valid_control_bytes() -> Vec<u8> {
        br#"{
            "id": "hybrid-source-test-config",
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

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(header_name, _)| header_name.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn explicit_config_builds_caller_driven_source_state_without_lifecycle_worker() {
        let transport = FakeTransport::new(vec![Ok(FfeSourceResponse {
            status_code: 200,
            headers: vec![("ETag".to_string(), "\"ufc-v1\"".to_string())],
            body: valid_control_bytes(),
        })]);
        let state = FfeSourceState::new_with_transport(explicit_config(), transport.clone());

        let outcome = state.poll_once();

        assert!(matches!(
            outcome,
            Ok(FfeSourcePollOutcome::Applied {
                status_code: 200,
                ..
            })
        ));
        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            header(&requests[0], "DD-API-KEY"),
            Some("explicit-test-key")
        );
        assert!(!format!("{:?}", explicit_config()).contains("explicit-test-key"));
        assert!(!source_has_hidden_start_or_worker());
    }

    #[test]
    fn poll_once_owns_etag_304_lkg_parse_apply_and_status_outcomes() {
        let transport = FakeTransport::new(vec![
            Ok(FfeSourceResponse {
                status_code: 200,
                headers: vec![("ETag".to_string(), "\"ufc-v1\"".to_string())],
                body: valid_control_bytes(),
            }),
            Ok(FfeSourceResponse {
                status_code: 304,
                headers: Vec::new(),
                body: Vec::new(),
            }),
            Ok(FfeSourceResponse {
                status_code: 200,
                headers: vec![("ETag".to_string(), "\"bad\"".to_string())],
                body: br#"{"flags":"#.to_vec(),
            }),
        ]);
        let state = FfeSourceState::new_with_transport(explicit_config(), transport.clone());

        let valid_control = state.poll_once();
        let unchanged_etag_304 = state.poll_once();
        let malformed_warm = state.poll_once();

        assert!(matches!(
            valid_control,
            Ok(FfeSourcePollOutcome::Applied { .. })
        ));
        assert!(matches!(
            unchanged_etag_304,
            Ok(FfeSourcePollOutcome::Unchanged {
                status_code: 304,
                ..
            })
        ));
        assert!(malformed_warm.is_err());
        assert!(state.is_ready());
        assert_eq!(state.last_etag().as_deref(), Some("\"ufc-v1\""));

        let requests = transport.requests();
        assert_eq!(header(&requests[1], "If-None-Match"), Some("\"ufc-v1\""));
        assert_eq!(header(&requests[2], "If-None-Match"), Some("\"ufc-v1\""));

        let context = EvaluationContext::new(Some(Str::from("user-123")), Arc::new(HashMap::new()));
        let details = state
            .resolve_value("valid-control", ExpectedFlagType::Boolean, &context)
            .expect("native source state should remain readable")
            .expect("warm last-known-good should evaluate");
        assert_eq!(details.variation_key.as_str(), "true");
    }

    #[test]
    fn retry_policy_retries_only_cdn_retryable_statuses() {
        let mut config = explicit_config();
        config.max_retries = 2;
        let retrying = FakeTransport::new(vec![
            Ok(FfeSourceResponse {
                status_code: 500,
                headers: Vec::new(),
                body: Vec::new(),
            }),
            Ok(FfeSourceResponse {
                status_code: 429,
                headers: Vec::new(),
                body: Vec::new(),
            }),
            Ok(FfeSourceResponse {
                status_code: 200,
                headers: vec![("ETag".to_string(), "\"ufc-v1\"".to_string())],
                body: valid_control_bytes(),
            }),
        ]);
        let state = FfeSourceState::new_with_transport(config, retrying.clone());

        let result = state.poll_once();

        assert!(matches!(
            result,
            Ok(FfeSourcePollOutcome::Applied { attempts: 3, .. })
        ));
        assert_eq!(retrying.requests().len(), 3);

        let non_retrying = FakeTransport::new(vec![Ok(FfeSourceResponse {
            status_code: 401,
            headers: Vec::new(),
            body: Vec::new(),
        })]);
        let state = FfeSourceState::new_with_transport(explicit_config(), non_retrying.clone());

        let result = state.poll_once();

        assert!(result.is_err());
        assert_eq!(non_retrying.requests().len(), 1);
    }

    fn source_has_hidden_start_or_worker() -> bool {
        let source = include_str!("source_state.rs");
        let forbidden = [
            ("pub fn ", "start"),
            ("thread::", "spawn"),
            ("std::env::", "var"),
        ];
        forbidden
            .iter()
            .any(|(prefix, suffix)| source.contains(&format!("{prefix}{suffix}")))
    }
}
