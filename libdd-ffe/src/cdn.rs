// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! CDN configuration fetching, independent of a tracer or sidecar lifecycle.
//!
//! Run the fetcher on the caller's Tokio runtime and cancel it before disposing
//! of its sink. HTTPS uses the crypto provider selected by `libdd-common`.

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
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("invalid CDN polling interval, request timeout, or client identity")]
pub struct InvalidConfig;

impl CdnConfig {
    /// Validate again at IPC boundaries; deserialization is not validation.
    pub fn validate(&self) -> Result<(), InvalidConfig> {
        if !self.endpoint.is_valid()
            || self.poll_interval.is_zero()
            || self.poll_interval > Duration::from_secs(3600)
            || self.request_timeout.is_zero()
            || self.request_timeout > Duration::from_secs(300)
            || self.language.is_empty()
            || self.language.len() > 128
            || self.version.is_empty()
            || self.version.len() > 256
            || http::HeaderValue::from_str(&self.language).is_err()
            || http::HeaderValue::from_str(&self.version).is_err()
        {
            return Err(InvalidConfig);
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
                HyperTransport::new(config.language, config.version),
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
    }

    #[test]
    fn deserialized_settings_cannot_bypass_endpoint_and_timing_validation() {
        let mut config = CdnConfig {
            endpoint: CdnEndpoint::custom("http://localhost/config").unwrap(),
            poll_interval: Duration::from_secs(30),
            request_timeout: Duration::from_secs(5),
            language: "php".into(),
            version: "1.27.0".into(),
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
    }
}
