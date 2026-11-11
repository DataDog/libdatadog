// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! CDN configuration fetching, independent of a tracer or sidecar lifecycle.
//!
//! Run the fetcher on the caller's Tokio runtime and cancel it before disposing
//! of its sink. Select `cdn,https` for ring or `cdn,fips` for FIPS HTTPS via
//! `libdd-networking`. CDN enables HTTP(S)_PROXY/NO_PROXY support independently of TLS.

mod endpoint;
mod poller;
mod transport;

pub use endpoint::{CdnEndpoint, EndpointError};
use poller::{PollState, Poller};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use transport::HyperTransport;

/// Complete subscription identity. Credentials participate in equality/hash so
/// different organizations can never share an authenticated fetcher or snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CdnConfig {
    pub endpoint: CdnEndpoint,
    pub poll_interval: Duration,
    pub request_timeout: Duration,
    pub language: String,
    pub version: String,
    /// Sent as `DD-Service` so the CDN can attribute usage to the service,
    /// cleaned up with the trace normalizer's `normalize_service`: `My Service`
    /// is sent as `my_service`, the name APM shows. A value with nothing usable
    /// left is left out of requests rather than rejected, so it never stops
    /// flag fetching.
    #[serde(default)]
    pub service: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidConfig {
    #[error("invalid CDN endpoint")]
    Endpoint,
    #[error("invalid CDN polling interval")]
    PollInterval,
    #[error("invalid CDN request timeout")]
    RequestTimeout,
    #[error("invalid CDN client language")]
    Language,
    #[error("invalid CDN client version")]
    Version,
}

impl CdnConfig {
    /// Validate again at IPC boundaries; deserialization is not validation.
    pub fn validate(&self) -> Result<(), InvalidConfig> {
        if !self.endpoint.is_valid() {
            return Err(InvalidConfig::Endpoint);
        }
        if self.poll_interval.is_zero() || self.poll_interval > Duration::from_secs(3600) {
            return Err(InvalidConfig::PollInterval);
        }
        if self.request_timeout.is_zero() || self.request_timeout > Duration::from_secs(300) {
            return Err(InvalidConfig::RequestTimeout);
        }
        if self.language.is_empty()
            || self.language.len() > 128
            || http::HeaderValue::from_str(&self.language).is_err()
        {
            return Err(InvalidConfig::Language);
        }
        if self.version.is_empty()
            || self.version.len() > 256
            || http::HeaderValue::from_str(&self.version).is_err()
        {
            return Err(InvalidConfig::Version);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("configuration publication failed")]
pub struct ApplyError;

/// The ETag advances only after `apply` succeeds. A failed poll never removes
/// the previously published configuration. `confirmed` includes accepted 304s.
pub trait ConfigurationSink: Send + Sync {
    fn apply(&self, configuration: crate::rules_based::Configuration) -> Result<(), ApplyError>;
    fn failed(&self) {}
    fn confirmed(&self) {}
}

pub struct CdnFetcher<S: ConfigurationSink> {
    poller: Poller<HyperTransport, S>,
}

impl<S: ConfigurationSink> CdnFetcher<S> {
    pub fn new(config: CdnConfig, sink: S) -> Result<Self, InvalidConfig> {
        config.validate()?;
        Ok(Self {
            poller: Poller::new(
                config.endpoint,
                config.poll_interval,
                config.request_timeout,
                HyperTransport::new(config.language, config.version, config.service),
                sink,
                Arc::new(PollState::default()),
            ),
        })
    }

    /// Poll immediately, then at the configured interval. Requests and retry
    /// waits are cancelled together; no more publications occur after return.
    pub async fn run(&mut self, cancellation: CancellationToken) {
        self.poller.run(cancellation).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_wire_format_preserves_identity_and_redacts_credentials() {
        let config = CdnConfig {
            endpoint: CdnEndpoint::managed("datadoghq.com", "production", "secret-key").unwrap(),
            poll_interval: Duration::from_secs(30),
            request_timeout: Duration::from_secs(5),
            language: "php".into(),
            version: "1.27.0".into(),
            service: Some("checkout".into()),
        };
        let decoded: CdnConfig =
            bincode::deserialize(&bincode::serialize(&config).unwrap()).unwrap();
        assert_eq!(decoded, config);
        assert!(decoded.validate().is_ok());
        assert!(!format!("{config:?}").contains("secret-key"));
        let mut other = config.clone();
        other.endpoint = CdnEndpoint::managed("datadoghq.com", "production", "other-key").unwrap();
        assert_ne!(other, config);
        other = config.clone();
        other.endpoint = CdnEndpoint::managed("datadoghq.com", "staging", "secret-key").unwrap();
        assert_ne!(other, config);
        // Each service gets its own fetcher.
        other = config.clone();
        other.service = Some("billing".into());
        assert_ne!(other, config);
    }

    #[test]
    fn deserialized_settings_cannot_bypass_endpoint_and_timing_validation() {
        let mut config = CdnConfig {
            endpoint: CdnEndpoint::custom("http://localhost/config").unwrap(),
            poll_interval: Duration::from_secs(30),
            request_timeout: Duration::from_secs(5),
            language: "php".into(),
            version: "1.27.0".into(),
            service: None,
        };
        let mut wire = serde_json::to_value(&config).unwrap();
        wire["endpoint"]["api_key"] = "secret".into();
        let invalid: CdnConfig = serde_json::from_value(wire).unwrap();
        assert!(
            invalid.validate().is_err(),
            "custom endpoints cannot acquire managed authentication through IPC"
        );
        config.poll_interval = Duration::ZERO;
        assert!(config.validate().is_err());
        config.poll_interval = Duration::from_secs(30);
        config.request_timeout = Duration::from_secs(301);
        assert!(config.validate().is_err());
        config.request_timeout = Duration::from_secs(5);
        config.language = "php\r\ninjected: value".into();
        assert!(config.validate().is_err());
        config.language = "php".into();
        // A service is cleaned up or left out of requests, never rejected.
        config.service = Some("php\r\ninjected: value".into());
        assert!(config.validate().is_ok());
    }

    #[test]
    fn a_subscription_without_a_service_still_deserializes() {
        let config = CdnConfig {
            endpoint: CdnEndpoint::custom("http://localhost/config").unwrap(),
            poll_interval: Duration::from_secs(30),
            request_timeout: Duration::from_secs(5),
            language: "php".into(),
            version: "1.27.0".into(),
            service: Some("checkout".into()),
        };
        let mut wire = serde_json::to_value(&config).unwrap();
        wire.as_object_mut().unwrap().remove("service");
        let decoded: CdnConfig = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded.service, None);
    }

    #[test]
    fn invalid_config_identifies_the_field_without_disclosing_values() {
        let config = CdnConfig {
            endpoint: CdnEndpoint::custom("http://secret-user:secret-pass@localhost/config")
                .unwrap(),
            poll_interval: Duration::from_secs(30),
            request_timeout: Duration::from_secs(5),
            language: "php".into(),
            version: "1.27.0".into(),
            service: None,
        };
        for (field, expected) in [
            ("endpoint", "invalid CDN endpoint"),
            ("poll_interval", "invalid CDN polling interval"),
            ("request_timeout", "invalid CDN request timeout"),
            ("language", "invalid CDN client language"),
            ("version", "invalid CDN client version"),
        ] {
            let mut invalid = config.clone();
            match field {
                "endpoint" => {
                    let mut wire = serde_json::to_value(&config.endpoint).unwrap();
                    wire["api_key"] = "secret-key".into();
                    invalid.endpoint = serde_json::from_value(wire).unwrap();
                }
                "poll_interval" => invalid.poll_interval = Duration::ZERO,
                "request_timeout" => invalid.request_timeout = Duration::from_secs(301),
                "language" => invalid.language = "secret-language\r\n".into(),
                "version" => invalid.version = "secret-version\r\n".into(),
                _ => unreachable!(),
            }
            let error = invalid.validate().unwrap_err();
            assert_eq!(error.to_string(), expected);
            assert!(!format!("{error:?}").contains("secret"));
        }
    }

    const SERVER_PATH: &str = "/api/v2/feature-flagging/config/rules-based/server";
    const EMPTY_CONFIG: &str = r#"{"data": {"type": "universal-flag-configuration", "attributes": {
        "createdAt": "2026-05-22T00:00:00.000Z", "format": "SERVER",
        "environment": {"name": "test"}, "flags": {}
    }}}"#;

    #[derive(Clone, Default)]
    struct CountingSink(Arc<std::sync::atomic::AtomicUsize>);

    impl ConfigurationSink for CountingSink {
        fn apply(
            &self,
            _configuration: crate::rules_based::Configuration,
        ) -> Result<(), ApplyError> {
            Ok(())
        }
        fn confirmed(&self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Polls `server`'s root as a custom CDN endpoint until `polls` succeed.
    async fn fetch(server: &httpmock::MockServer, service: Option<&str>, polls: usize) {
        let config = CdnConfig {
            endpoint: CdnEndpoint::custom(&server.base_url()).unwrap(),
            poll_interval: Duration::from_millis(10),
            request_timeout: Duration::from_secs(1),
            language: "php".into(),
            version: "1.27.0".into(),
            service: service.map(Into::into),
        };
        let sink = CountingSink::default();
        let mut fetcher = CdnFetcher::new(config, sink.clone()).expect("valid configuration");
        let cancellation = CancellationToken::new();
        let task = tokio::spawn({
            let cancellation = cancellation.clone();
            async move { fetcher.run(cancellation).await }
        });
        let confirmed = tokio::time::timeout(Duration::from_secs(5), async {
            while sink.0.load(std::sync::atomic::Ordering::Relaxed) < polls {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        cancellation.cancel();
        task.await.unwrap();
        assert!(confirmed.is_ok(), "{polls} polls did not succeed");
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore = "requires native HTTP")]
    async fn every_poll_sends_the_service_in_dd_service() {
        let server = httpmock::MockServer::start_async().await;
        let first = server
            .mock_async(|when, then| {
                when.method("GET")
                    .path(SERVER_PATH)
                    .header("DD-Service", "checkout")
                    .header_missing("If-None-Match");
                then.status(200).header("ETag", "\"v1\"").body(EMPTY_CONFIG);
            })
            .await;
        let revalidated = server
            .mock_async(|when, then| {
                when.method("GET")
                    .path(SERVER_PATH)
                    .header("DD-Service", "checkout")
                    .header("If-None-Match", "\"v1\"");
                then.status(304);
            })
            .await;

        fetch(&server, Some("checkout"), 2).await;

        assert_eq!(first.calls_async().await, 1);
        assert!(revalidated.calls_async().await >= 1);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore = "requires native HTTP")]
    async fn a_service_the_cdn_would_drop_is_sent_cleaned_up_in_dd_service() {
        let server = httpmock::MockServer::start_async().await;
        let cleaned = server
            .mock_async(|when, then| {
                when.method("GET")
                    .path(SERVER_PATH)
                    .header("DD-Service", "my_service");
                then.status(200).body(EMPTY_CONFIG);
            })
            .await;

        fetch(&server, Some("My Service"), 2).await;

        assert!(cleaned.calls_async().await >= 2);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore = "requires native HTTP")]
    async fn a_missing_service_omits_dd_service_and_keeps_fetching() {
        for service in [None, Some("")] {
            let server = httpmock::MockServer::start_async().await;
            let without_service = server
                .mock_async(|when, then| {
                    when.method("GET")
                        .path(SERVER_PATH)
                        .header_missing("DD-Service");
                    then.status(200).body(EMPTY_CONFIG);
                })
                .await;

            fetch(&server, service, 1).await;

            assert!(without_service.calls_async().await >= 1, "{service:?}");
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore = "requires native HTTP")]
    async fn a_service_with_nothing_left_once_cleaned_up_omits_dd_service_and_keeps_fetching() {
        for service in ["支付", "___"] {
            let server = httpmock::MockServer::start_async().await;
            let without_service = server
                .mock_async(|when, then| {
                    when.method("GET")
                        .path(SERVER_PATH)
                        .header_missing("DD-Service");
                    then.status(200).body(EMPTY_CONFIG);
                })
                .await;
            crate::test_logging::start_log_capture();

            fetch(&server, Some(service), 2).await;

            assert!(without_service.calls_async().await >= 2, "{service:?}");
            // Logged once per fetcher, not on every poll.
            let skipped = crate::test_logging::captured_logs()
                .into_iter()
                .filter(|log| log.message.contains("DD-Service header not sent"))
                .count();
            assert_eq!(skipped, 1, "{service:?}");
        }
    }
}
