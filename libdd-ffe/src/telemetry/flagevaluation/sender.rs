// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{encode_flag_evaluation_payloads, FfeFlagEvaluationBatch};
use http::header::{HeaderValue, InvalidHeaderValue};
use http::uri::PathAndQuery;
use http::Method;
use libdd_capabilities::{Bytes, HttpClientCapability, SleepCapability};
use libdd_common::Endpoint;
use std::time::Duration;
use thiserror::Error;

/// EVP proxy path for FFE flag evaluation intake.
pub const EVP_FLAGEVALUATION_PATH: &str = "/evp_proxy/v2/api/v2/flagevaluation";
/// EVP subdomain header name.
pub const EVP_SUBDOMAIN_HEADER: &str = "X-Datadog-EVP-Subdomain";
/// EVP subdomain that routes requests to event-platform intake.
pub const EVP_SUBDOMAIN_VALUE: &str = "event-platform-intake";
const EVP_ORIGIN_HEADER: &str = "DD-EVP-ORIGIN";
const EVP_ORIGIN_VERSION_HEADER: &str = "DD-EVP-ORIGIN-VERSION";
/// Agent EVP proxy uncompressed request-body limit.
///
/// Revalidated against `DataDog/datadog-agent` on 2026-07-01:
/// `pkg/config/setup/apm.go` defaults `evp_proxy_config.max_payload_size` to
/// `10*1024*1024`; `comp/trace/config/impl/setup.go` copies that value into
/// `EVPProxy.MaxPayloadSize`; and `pkg/trace/api/evp_proxy.go` enforces it via
/// `apiutil.NewLimitedReader`.
pub const EVP_PAYLOAD_SIZE_LIMIT: usize = 10 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum FlagEvaluationEvpSendConfigError {
    #[error("EVP origin must not be empty")]
    EmptyOrigin,
    #[error("EVP origin version must not be empty")]
    EmptyOriginVersion,
    #[error("EVP origin is not a valid HTTP header value")]
    InvalidOrigin(#[source] InvalidHeaderValue),
    #[error("EVP origin version is not a valid HTTP header value")]
    InvalidOriginVersion(#[source] InvalidHeaderValue),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FlagEvaluationEvpSendConfig {
    user_agent: String,
    origin: HeaderValue,
    origin_version: HeaderValue,
    payload_size_limit: usize,
}

impl FlagEvaluationEvpSendConfig {
    /// Creates a send configuration with required producer identity metadata.
    ///
    /// Origin and origin version must both contain a non-whitespace character
    /// and be valid HTTP header values. Validating them here ensures every
    /// request built from this configuration carries usable producer identity
    /// metadata. Accepted values are stored without normalization.
    pub fn new(
        user_agent: impl Into<String>,
        origin: impl Into<String>,
        origin_version: impl Into<String>,
    ) -> Result<Self, FlagEvaluationEvpSendConfigError> {
        let origin = origin.into();
        if origin.trim().is_empty() {
            return Err(FlagEvaluationEvpSendConfigError::EmptyOrigin);
        }
        let origin = HeaderValue::try_from(origin.as_str())
            .map_err(FlagEvaluationEvpSendConfigError::InvalidOrigin)?;

        let origin_version = origin_version.into();
        if origin_version.trim().is_empty() {
            return Err(FlagEvaluationEvpSendConfigError::EmptyOriginVersion);
        }
        let origin_version = HeaderValue::try_from(origin_version.as_str())
            .map_err(FlagEvaluationEvpSendConfigError::InvalidOriginVersion)?;

        Ok(Self {
            user_agent: user_agent.into(),
            origin,
            origin_version,
            payload_size_limit: EVP_PAYLOAD_SIZE_LIMIT,
        })
    }

    pub fn with_payload_size_limit(mut self, payload_size_limit: usize) -> Self {
        self.payload_size_limit = payload_size_limit;
        self
    }
}

/// Build the Agent EVP proxy endpoint for FFE flag evaluation intake.
///
/// This preserves the base endpoint's scheme, authority, timeout, and test
/// token while swapping the path to `/evp_proxy/v2/api/v2/flagevaluation`.
/// Agentless submission is not wired yet, so API-key endpoints are rejected
/// until this sender grows direct intake routing.
pub fn flagevaluation_agent_proxy_endpoint(base: &Endpoint) -> Option<Endpoint> {
    if base.api_key.is_some() {
        return None;
    }

    let mut parts = base.url.clone().into_parts();
    parts.path_and_query = Some(PathAndQuery::from_static(EVP_FLAGEVALUATION_PATH));
    let url = http::Uri::from_parts(parts).ok()?;
    Some(Endpoint {
        url,
        ..base.clone()
    })
}

/// POST a structured FFE flag evaluation batch through the Agent EVP proxy.
///
/// Returns the payload-build result after all generated payloads have been
/// attempted. HTTP failures are logged and dropped, matching the fire-and-forget
/// SDK behavior.
pub async fn send_flag_evaluation_batch<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    batch: FfeFlagEvaluationBatch,
    config: &FlagEvaluationEvpSendConfig,
) -> Option<super::FlagEvaluationEvpPayloadBuildResult> {
    let result = match encode_flag_evaluation_payloads(batch, config.payload_size_limit) {
        Ok(result) => result,
        Err(e) => {
            log::debug!("ffe flagevaluation sender failed to encode batch payload: {e:?}");
            return None;
        }
    };

    if result.dropped_oversized_rows > 0 {
        log::warn!(
            "ffe flagevaluation sender dropped {} flag evaluation row(s) because they exceeded the {} byte EVP payload limit after degradation",
            result.dropped_oversized_rows,
            config.payload_size_limit
        );
    }

    for payload in &result.payloads {
        send_payload(client, endpoint, payload.clone(), config).await;
    }

    Some(result)
}

async fn send_payload<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    payload: String,
    config: &FlagEvaluationEvpSendConfig,
) {
    let builder = match endpoint.to_request_builder(&config.user_agent) {
        Ok(b) => b,
        Err(e) => {
            log::debug!("ffe flagevaluation sender failed to build request: {e:?}");
            return;
        }
    };

    let req = match builder
        .method(Method::POST)
        .header("Content-Type", "application/json")
        .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
        .header(EVP_ORIGIN_HEADER, config.origin.clone())
        .header(EVP_ORIGIN_VERSION_HEADER, config.origin_version.clone())
        .body(Bytes::from(payload))
    {
        Ok(r) => r,
        Err(e) => {
            log::debug!("ffe flagevaluation sender failed to construct request body: {e:?}");
            return;
        }
    };

    let timeout = Duration::from_millis(endpoint.timeout_ms);
    let response = tokio::select! {
        biased;
        result = client.request(req) => result,
        _ = client.sleep(timeout) => {
            log::debug!("ffe flagevaluation sender request timed out after {timeout:?}");
            return;
        }
    };

    match response {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let body_preview = truncate(resp.body().as_ref(), 256);
                log::warn!("ffe flagevaluation sender non-2xx response {status}: {body_preview}");
            } else {
                log::debug!(
                    "ffe flagevaluation sender sent flag evaluation batch, status={status}"
                );
            }
        }
        Err(e) => {
            log::debug!("ffe flagevaluation sender request failed: {e:?}");
        }
    }
}

fn truncate(bytes: &[u8], cap: usize) -> String {
    let take = bytes.len().min(cap);
    String::from_utf8_lossy(&bytes[..take]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::FfeTelemetryContext;
    use httpmock::MockServer;
    use libdd_capabilities::{HttpError, MaybeSend};
    use libdd_capabilities_impl::NativeCapabilities;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::future;
    use std::sync::{Mutex, Once};

    use super::super::{
        AllocationKey, ContextDD, EvalError, FfeFlagEvaluationEvent, FlagEvalEventContext, FlagKey,
        TargetingRuleKey, VariantKey, MAX_EVENTS_PER_POST,
    };

    #[derive(Clone)]
    struct CapturedLog {
        level: log::Level,
        message: String,
    }

    struct CapturingLogger {
        records: Mutex<Vec<CapturedLog>>,
    }

    impl log::Log for CapturingLogger {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() <= log::Level::Debug
        }

        fn log(&self, record: &log::Record<'_>) {
            if self.enabled(record.metadata()) {
                self.records.lock().unwrap().push(CapturedLog {
                    level: record.level(),
                    message: record.args().to_string(),
                });
            }
        }

        fn flush(&self) {}
    }

    static TEST_LOGGER: CapturingLogger = CapturingLogger {
        records: Mutex::new(Vec::new()),
    };
    static INIT_LOGGER: Once = Once::new();

    fn start_log_capture() {
        INIT_LOGGER.call_once(|| {
            let _ = log::set_logger(&TEST_LOGGER);
            log::set_max_level(log::LevelFilter::Debug);
        });
        TEST_LOGGER.records.lock().unwrap().clear();
    }

    fn captured_logs() -> Vec<CapturedLog> {
        TEST_LOGGER.records.lock().unwrap().clone()
    }

    fn context() -> FfeTelemetryContext {
        FfeTelemetryContext {
            service: "svc".to_owned(),
            env: "prod".to_owned(),
            version: "1".to_owned(),
        }
    }

    fn full_event() -> FfeFlagEvaluationEvent {
        FfeFlagEvaluationEvent {
            timestamp: 1_700_000_000_000,
            flag: FlagKey {
                key: "my-flag".to_owned(),
            },
            first_evaluation: 1_699_999_990_000,
            last_evaluation: 1_700_000_000_000,
            evaluation_count: 42,
            variant: Some(VariantKey {
                key: "on".to_owned(),
            }),
            allocation: Some(AllocationKey {
                key: "alloc-a".to_owned(),
            }),
            targeting_key: Some("user-123".to_owned()),
            targeting_rule: Some(TargetingRuleKey {
                key: "rule-1".to_owned(),
            }),
            context: Some(FlagEvalEventContext {
                evaluation: Some(
                    serde_json::to_string(&{
                        let mut m = BTreeMap::new();
                        m.insert("plan".to_owned(), json!("premium"));
                        m
                    })
                    .unwrap(),
                ),
                dd: Some(ContextDD {
                    service: "frontend".to_owned(),
                }),
            }),
            error: Some(EvalError {
                message: "boom".to_owned(),
            }),
            runtime_default_used: true,
        }
    }

    fn endpoint_for(server: &MockServer) -> Endpoint {
        Endpoint {
            url: server.url("/").parse().unwrap(),
            ..Endpoint::default()
        }
    }

    fn send_config() -> FlagEvaluationEvpSendConfig {
        FlagEvaluationEvpSendConfig::new("libdd-ffe-test/0.0.0", "libdd-ffe-test-origin", "1.2.3")
            .unwrap()
    }

    fn batch() -> FfeFlagEvaluationBatch {
        FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![full_event()],
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn posts_to_evp_proxy() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
                    .header("content-type", "application/json")
                    .header("user-agent", "libdd-ffe-test/0.0.0")
                    .header(EVP_ORIGIN_HEADER, "libdd-ffe-test-origin")
                    .header(EVP_ORIGIN_VERSION_HEADER, "1.2.3");
                then.status(202);
            })
            .await;

        let ep = flagevaluation_agent_proxy_endpoint(&endpoint_for(&server)).unwrap();
        let client = NativeCapabilities::new_client();

        send_flag_evaluation_batch(&client, &ep, batch(), &send_config()).await;

        mock.assert_async().await;
        assert_eq!(mock.calls_async().await, 1);
    }

    #[test]
    fn send_config_rejects_empty_whitespace_or_invalid_producer_identity() {
        for empty_origin in ["", " ", "\t", " \t "] {
            assert!(matches!(
                FlagEvaluationEvpSendConfig::new("user-agent", empty_origin, "1.2.3"),
                Err(FlagEvaluationEvpSendConfigError::EmptyOrigin)
            ));
        }
        for empty_origin_version in ["", " ", "\t", " \t "] {
            assert!(matches!(
                FlagEvaluationEvpSendConfig::new("user-agent", "origin", empty_origin_version),
                Err(FlagEvaluationEvpSendConfigError::EmptyOriginVersion)
            ));
        }
        assert!(matches!(
            FlagEvaluationEvpSendConfig::new("user-agent", "invalid\norigin", "1.2.3"),
            Err(FlagEvaluationEvpSendConfigError::InvalidOrigin(_))
        ));
        assert!(matches!(
            FlagEvaluationEvpSendConfig::new("user-agent", "origin", "invalid\nversion"),
            Err(FlagEvaluationEvpSendConfigError::InvalidOriginVersion(_))
        ));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn splits_large_batches_before_posting() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
                    .header("content-type", "application/json");
                then.status(202);
            })
            .await;

        let ep = flagevaluation_agent_proxy_endpoint(&endpoint_for(&server)).unwrap();
        let client = NativeCapabilities::new_client();
        let mut batch = batch();
        let event = batch.flag_evaluations[0].clone();
        batch.flag_evaluations = vec![event; MAX_EVENTS_PER_POST * 2 + 1];

        send_flag_evaluation_batch(&client, &ep, batch, &send_config()).await;

        mock.assert_calls_async(3).await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn send_batch_splits_posts_by_encoded_byte_limit() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
                    .header("content-type", "application/json");
                then.status(202);
            })
            .await;

        let ep = flagevaluation_agent_proxy_endpoint(&endpoint_for(&server)).unwrap();
        let client = NativeCapabilities::new_client();
        let mut batch = batch();
        let event = batch.flag_evaluations[0].clone();
        batch.flag_evaluations = vec![event; 3];
        let one_event_limit = encode_flag_evaluation_payloads(
            FfeFlagEvaluationBatch {
                context: context(),
                flag_evaluations: vec![batch.flag_evaluations[0].clone()],
            },
            usize::MAX,
        )
        .unwrap()
        .payloads
        .into_iter()
        .next()
        .unwrap()
        .len();
        let config = send_config().with_payload_size_limit(one_event_limit);

        send_flag_evaluation_batch(&client, &ep, batch, &config).await;

        mock.assert_calls_async(3).await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn non_2xx_does_not_panic() {
        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH);
                then.status(500).body("intake overloaded");
            })
            .await;

        let ep = flagevaluation_agent_proxy_endpoint(&endpoint_for(&server)).unwrap();
        let client = NativeCapabilities::new_client();

        send_flag_evaluation_batch(&client, &ep, batch(), &send_config()).await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn delivery_loss_paths_log_at_warn_level() {
        start_log_capture();

        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH);
                then.status(500).body("intake overloaded");
            })
            .await;
        let ep = flagevaluation_agent_proxy_endpoint(&endpoint_for(&server)).unwrap();
        let client = NativeCapabilities::new_client();

        send_flag_evaluation_batch(&client, &ep, batch(), &send_config()).await;

        let mut oversized = full_event();
        oversized.flag.key = "x".repeat(1024);
        let result = send_flag_evaluation_batch(
            &client,
            &ep,
            FfeFlagEvaluationBatch {
                context: context(),
                flag_evaluations: vec![oversized],
            },
            &send_config().with_payload_size_limit(128),
        )
        .await
        .expect("payload build should succeed");
        assert_eq!(result.dropped_oversized_rows, 42);

        let records = captured_logs();
        for pattern in [
            "ffe flagevaluation sender non-2xx response 500",
            "ffe flagevaluation sender dropped 42 flag evaluation row(s)",
        ] {
            assert!(
                records
                    .iter()
                    .any(|record| record.level == log::Level::Warn
                        && record.message.contains(pattern)),
                "expected warn log containing {pattern:?}; got {:?}",
                records
                    .iter()
                    .map(|record| (&record.level, &record.message))
                    .collect::<Vec<_>>()
            );
            assert!(
                !records
                    .iter()
                    .any(|record| record.level == log::Level::Debug
                        && record.message.contains(pattern)),
                "expected no debug log containing {pattern:?}"
            );
        }
    }

    #[cfg_attr(miri, ignore)] // hanging HTTP timeout is prohibitively slow under Miri
    #[tokio::test]
    async fn timeout_returns_without_waiting_for_http_response() {
        let ep = Endpoint {
            url: "http://localhost:8126".parse().unwrap(),
            timeout_ms: 1,
            ..Endpoint::default()
        };

        send_flag_evaluation_batch(&HangingCapabilities, &ep, batch(), &send_config()).await;
    }

    #[test]
    fn endpoint_preserves_authority_overrides_path() {
        let base = Endpoint {
            url: "http://agent.internal:8126/v0.4/traces".parse().unwrap(),
            ..Endpoint::default()
        };
        let ep = flagevaluation_agent_proxy_endpoint(&base).unwrap();
        assert_eq!(ep.url.scheme_str(), Some("http"));
        assert_eq!(ep.url.authority().unwrap().as_str(), "agent.internal:8126");
        assert_eq!(ep.url.path(), EVP_FLAGEVALUATION_PATH);
    }

    #[test]
    fn endpoint_rejects_agentless() {
        let base = Endpoint {
            url: "https://trace.agent.datadoghq.com/v0.4/traces"
                .parse()
                .unwrap(),
            api_key: Some("api-key".into()),
            ..Endpoint::default()
        };
        assert!(flagevaluation_agent_proxy_endpoint(&base).is_none());
    }

    #[derive(Clone, Debug)]
    struct HangingCapabilities;

    impl HttpClientCapability for HangingCapabilities {
        fn new_client() -> Self {
            Self
        }

        fn new_without_connection_pooling() -> Self {
            Self
        }

        fn request(
            &self,
            _req: http::Request<Bytes>,
        ) -> impl future::Future<Output = Result<http::Response<Bytes>, HttpError>> + MaybeSend
        {
            future::pending()
        }
    }

    impl SleepCapability for HangingCapabilities {
        fn new() -> Self {
            Self
        }

        async fn sleep(&self, _duration: Duration) {}
    }
}
