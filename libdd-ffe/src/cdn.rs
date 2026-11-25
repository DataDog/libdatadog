// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! CDN configuration fetching, independent of a tracer or sidecar lifecycle.
//!
//! Run the fetcher on the caller's Tokio runtime and cancel it before disposing
//! of its sink. CDN requests are proxied only when the subscription explicitly
//! configures one (see [`CdnProxy`]); the fetcher never reads the environment.

mod endpoint;
mod poller;
mod transport;

pub use endpoint::{CdnEndpoint, EndpointError};
use poller::{PollState, Poller};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use transport::HyperTransport;

/// Explicit proxy routing for CDN requests.
///
/// The fetcher never reads the environment; the caller decides how to source
/// this — including proxy environment variables — and passes it explicitly.
/// Debug redacts the URL, which may carry credentials.
#[derive(Clone, Default, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CdnProxy {
    /// Proxy URL (`http://` or `https://` with a host); credentials allowed
    /// (`http://user:pass@proxy:8080`).
    #[serde(default)]
    pub url: Option<String>,
    /// Comma-separated entries that bypass the proxy, `NO_PROXY` syntax.
    #[serde(default)]
    pub no_proxy: Option<String>,
}

impl std::fmt::Debug for CdnProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CdnProxy")
            .field("url", &self.url.as_ref().map(|_| "[REDACTED]"))
            .field("no_proxy", &self.no_proxy)
            .finish()
    }
}

/// Complete subscription identity. Credentials participate in equality/hash so
/// different organizations can never share an authenticated fetcher or snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CdnConfig {
    pub endpoint: CdnEndpoint,
    pub poll_interval: Duration,
    pub request_timeout: Duration,
    pub language: String,
    pub version: String,
    /// The service name. Requests send it in the `DD-Service` header, so the
    /// CDN can attribute usage to the service. The header has the name from the
    /// trace normalizer (`libdd-trace-normalization`): `My Service` becomes
    /// `my_service`. If that name is empty or the normalizer fallback
    /// `unnamed-service` (for example from `___`), requests have no header.
    /// This is not a validation error, so flag fetching continues.
    #[serde(default)]
    pub service: Option<String>,
    /// Explicit proxy routing for CDN requests; direct when unset.
    #[serde(default)]
    pub proxy: CdnProxy,
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
    #[error("invalid CDN proxy")]
    Proxy,
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
        if let Some(proxy_url) = &self.proxy.url {
            let Ok(url) = url::Url::parse(proxy_url) else {
                return Err(InvalidConfig::Proxy);
            };
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err(InvalidConfig::Proxy);
            }
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
                HyperTransport::new(
                    config.language,
                    config.version,
                    config.service,
                    config.proxy,
                ),
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
            proxy: CdnProxy {
                url: Some("http://secret-user:secret-pass@proxy.example:8080".into()),
                no_proxy: Some("localhost".into()),
            },
        };
        let decoded: CdnConfig =
            bincode::deserialize(&bincode::serialize(&config).unwrap()).unwrap();
        assert_eq!(decoded, config);
        assert!(decoded.validate().is_ok());
        assert!(!format!("{config:?}").contains("secret-key"));
        assert!(
            !format!("{config:?}").contains("secret-pass"),
            "proxy credentials must be redacted from Debug"
        );
        let mut other = config.clone();
        other.endpoint = CdnEndpoint::managed("datadoghq.com", "production", "other-key").unwrap();
        assert_ne!(other, config);
        other = config.clone();
        other.endpoint = CdnEndpoint::managed("datadoghq.com", "staging", "secret-key").unwrap();
        assert_ne!(other, config);
        // Each service has its own fetcher.
        other = config.clone();
        other.service = Some("billing".into());
        assert_ne!(other, config);
        // Each proxy routing has its own fetcher.
        other = config.clone();
        other.proxy.url = Some("http://proxy.example:3128".into());
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
            proxy: CdnProxy::default(),
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
        // validate() does not reject a service. A bad service only removes
        // the header.
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
            proxy: CdnProxy::default(),
        };
        let mut wire = serde_json::to_value(&config).unwrap();
        wire.as_object_mut().unwrap().remove("service");
        // Subscriptions serialized by older senders have no proxy field.
        wire.as_object_mut().unwrap().remove("proxy");
        let decoded: CdnConfig = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded.service, None);
        assert_eq!(decoded.proxy, CdnProxy::default());
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
            proxy: CdnProxy::default(),
        };
        for (field, expected) in [
            ("endpoint", "invalid CDN endpoint"),
            ("poll_interval", "invalid CDN polling interval"),
            ("request_timeout", "invalid CDN request timeout"),
            ("language", "invalid CDN client language"),
            ("version", "invalid CDN client version"),
            ("proxy", "invalid CDN proxy"),
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
                "proxy" => invalid.proxy.url = Some("secret-proxy://not-http\r\n".into()),
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

    /// Polls the root of `server` as a custom CDN endpoint. Stops after
    /// `polls` successful polls.
    async fn fetch(server: &httpmock::MockServer, service: Option<&str>, polls: usize) {
        fetch_url(&server.base_url(), service, polls).await;
    }

    /// Polls `base_url` as a custom CDN endpoint. Stops after `polls`
    /// successful polls.
    async fn fetch_url(base_url: &str, service: Option<&str>, polls: usize) {
        let config = CdnConfig {
            endpoint: CdnEndpoint::custom(base_url).unwrap(),
            poll_interval: Duration::from_millis(10),
            request_timeout: Duration::from_secs(1),
            language: "php".into(),
            version: "1.27.0".into(),
            service: service.map(Into::into),
            proxy: CdnProxy::default(),
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
    async fn a_service_is_sent_normalized_in_dd_service() {
        for (service, normalized_name) in [
            ("My Service", "my_service"),
            ("Default Web Site/Shop", "default_web_site/shop"),
        ] {
            let server = httpmock::MockServer::start_async().await;
            let normalized = server
                .mock_async(|when, then| {
                    when.method("GET")
                        .path(SERVER_PATH)
                        .header("DD-Service", normalized_name);
                    then.status(200).body(EMPTY_CONFIG);
                })
                .await;

            fetch(&server, Some(service), 2).await;

            assert!(normalized.calls_async().await >= 2, "{service:?}");
        }
    }

    /// Serves `EMPTY_CONFIG` on a local socket, and records the raw bytes of
    /// the `DD-Service` value of each request. httpmock cannot match a header
    /// value that is not ASCII.
    fn raw_server() -> (String, Arc<std::sync::Mutex<Vec<Vec<u8>>>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = received.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let service = request.split(|byte| *byte == b'\n').find_map(|line| {
                    let line = line.strip_suffix(b"\r").unwrap_or(line);
                    let (name, value) = line.split_at(line.iter().position(|b| *b == b':')?);
                    name.eq_ignore_ascii_case(b"dd-service")
                        .then(|| value[1..].trim_ascii().to_vec())
                });
                recorded.lock().unwrap().extend(service);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{EMPTY_CONFIG}",
                    EMPTY_CONFIG.len()
                );
            }
        });
        (base_url, received)
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore = "requires native HTTP")]
    async fn a_non_ascii_service_is_sent_as_utf8_and_fetching_continues() {
        for service in ["café-api", "支付"] {
            let (base_url, received) = raw_server();

            fetch_url(&base_url, Some(service), 2).await;

            let received = received.lock().unwrap();
            assert!(received.len() >= 2, "{service:?}");
            assert!(
                received.iter().all(|value| value == service.as_bytes()),
                "{service:?}"
            );
        }
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
    async fn a_service_without_a_normalized_name_omits_dd_service_and_keeps_fetching() {
        // `___` becomes the normalizer fallback `unnamed-service`.
        for service in ["___", "!!!"] {
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
            // The fetcher logs once, not on each poll.
            let skipped = crate::test_logging::captured_logs()
                .into_iter()
                .filter(|log| log.message.contains("DD-Service header not sent"))
                .count();
            assert_eq!(skipped, 1, "{service:?}");
        }
    }
}
