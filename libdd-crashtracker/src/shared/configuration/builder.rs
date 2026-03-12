// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::{default_signals, shared::constants, signal_from_signum};
use libdd_common::Endpoint;
use std::borrow::Cow;
use std::time::Duration;

use super::{CrashtrackerConfiguration, StacktraceCollection};

#[derive(Debug, Default)]
pub struct CrashtrackerConfigurationBuilder {
    additional_files: Vec<String>,
    create_alt_stack: bool,
    demangle_names: bool,
    endpoint_url: Option<String>,
    endpoint_api_key: Option<String>,
    endpoint_timeout_ms: Option<u64>,
    endpoint_test_token: Option<String>,
    endpoint_use_system_resolver: bool,
    resolve_frames: StacktraceCollection,
    signals: Vec<i32>,
    timeout: Option<Duration>,
    unix_socket_path: Option<String>,
    use_alt_stack: bool,
}

impl CrashtrackerConfigurationBuilder {
    pub fn additional_files(mut self, files: Vec<String>) -> Self {
        self.additional_files = files;
        self
    }

    pub fn create_alt_stack(mut self, create: bool) -> Self {
        self.create_alt_stack = create;
        self
    }

    pub fn use_alt_stack(mut self, use_it: bool) -> Self {
        self.use_alt_stack = use_it;
        self
    }

    pub fn demangle_names(mut self, demangle: bool) -> Self {
        self.demangle_names = demangle;
        self
    }

    pub fn endpoint_url(mut self, url: &str) -> Self {
        if !url.is_empty() {
            self.endpoint_url = Some(url.to_string());
        }
        self
    }

    pub fn endpoint_api_key(mut self, api_key: &str) -> Self {
        self.endpoint_api_key = Some(api_key.to_string());
        self
    }

    pub fn endpoint_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.endpoint_timeout_ms = Some(timeout_ms);
        self
    }

    pub fn endpoint_test_token(mut self, test_token: &str) -> Self {
        self.endpoint_test_token = Some(test_token.to_string());
        self
    }

    pub fn endpoint_use_system_resolver(mut self, use_system_resolver: bool) -> Self {
        self.endpoint_use_system_resolver = use_system_resolver;
        self
    }

    pub fn resolve_frames(mut self, resolve: StacktraceCollection) -> Self {
        self.resolve_frames = resolve;
        self
    }

    pub fn signals(mut self, signals: Vec<i32>) -> Self {
        self.signals = signals;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn unix_socket_path(mut self, path: String) -> Self {
        self.unix_socket_path = Some(path);
        self
    }

    pub fn build(self) -> anyhow::Result<CrashtrackerConfiguration> {
        // Requesting to create, but not use, the altstack is considered paradoxical.
        anyhow::ensure!(
            !self.create_alt_stack || self.use_alt_stack,
            "Cannot create an altstack without using it"
        );
        let timeout = self
            .timeout
            .unwrap_or(constants::DD_CRASHTRACK_DEFAULT_TIMEOUT);
        let endpoint = self
            .endpoint_url
            .map(|url| {
                Ok::<Endpoint, anyhow::Error>(Endpoint {
                    url: libdd_common::parse_uri(&url)?,
                    api_key: self.endpoint_api_key.map(Cow::Owned),
                    timeout_ms: self
                        .endpoint_timeout_ms
                        .unwrap_or(Endpoint::DEFAULT_TIMEOUT),
                    test_token: self.endpoint_test_token.map(Cow::Owned),
                    use_system_resolver: self.endpoint_use_system_resolver,
                })
            })
            .transpose()?;

        let mut signals = self.signals;
        if signals.is_empty() {
            signals = default_signals();
        } else {
            // Ensure we don't have double elements in the signals list.
            let before_len = signals.len();
            signals.sort();
            signals.dedup();
            anyhow::ensure!(
                before_len == signals.len(),
                "Signals contained duplicate elements"
            );
            // Ensure that all signal values translate to a valid signum
            signals
                .iter()
                .try_for_each(|x| signal_from_signum(*x).map(|_| ()))?;
        }

        // Note: don't check the receiver socket upfront, since a configuration can be interned
        // before the receiver is started when using an async-receiver.
        Ok(CrashtrackerConfiguration {
            additional_files: self.additional_files,
            create_alt_stack: self.create_alt_stack,
            use_alt_stack: self.use_alt_stack,
            endpoint,
            resolve_frames: self.resolve_frames,
            signals,
            timeout,
            unix_socket_path: self.unix_socket_path,
            demangle_names: self.demangle_names,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{default_signals, shared::constants};
    use std::time::Duration;

    #[test]
    fn test_build_defaults() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder().build()?;
        assert!(config.additional_files().is_empty());
        assert!(!config.create_alt_stack());
        assert!(!config.use_alt_stack());
        assert!(!config.demangle_names());
        assert!(config.endpoint().is_none());
        assert_eq!(config.resolve_frames(), StacktraceCollection::Disabled);
        assert_eq!(config.signals(), &default_signals());
        assert_eq!(config.timeout(), constants::DD_CRASHTRACK_DEFAULT_TIMEOUT);
        assert!(config.unix_socket_path().is_none());
        Ok(())
    }

    #[test]
    fn test_create_alt_stack_without_use_fails() {
        let result = CrashtrackerConfiguration::builder()
            .create_alt_stack(true)
            .use_alt_stack(false)
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn test_create_and_use_alt_stack_succeeds() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder()
            .create_alt_stack(true)
            .use_alt_stack(true)
            .build()?;
        assert!(config.create_alt_stack());
        assert!(config.use_alt_stack());
        Ok(())
    }

    #[test]
    fn test_endpoint_empty_url() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder()
            .endpoint_url("")
            .build()?;
        assert!(config.endpoint().is_none());
        Ok(())
    }

    #[test]
    fn test_endpoint_file_url() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder()
            .endpoint_url("file:///tmp/crashreport.json")
            .build()?;
        let endpoint = config.endpoint().as_ref().expect("endpoint should be set");
        assert_eq!(endpoint.url.scheme_str(), Some("file"));
        Ok(())
    }

    #[test]
    fn test_endpoint_http_url() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder()
            .endpoint_url("http://localhost:8126/api/v2/profile")
            .build()?;
        let endpoint = config.endpoint().as_ref().expect("endpoint should be set");
        assert_eq!(endpoint.url.scheme_str(), Some("http"));
        assert_eq!(endpoint.url.port().unwrap().as_u16(), 8126);
        assert_eq!(endpoint.url.host(), Some("localhost"));
        Ok(())
    }

    #[test]
    fn test_endpoint_default_timeout_ms() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder()
            .endpoint_url("http://localhost:8126")
            .build()?;
        let endpoint = config.endpoint().as_ref().expect("endpoint should be set");
        assert_eq!(endpoint.url.port().unwrap().as_u16(), 8126);
        assert_eq!(endpoint.timeout_ms, Endpoint::DEFAULT_TIMEOUT);
        Ok(())
    }

    #[test]
    fn test_endpoint_custom_timeout_ms() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder()
            .endpoint_url("http://localhost:8126")
            .endpoint_timeout_ms(1234)
            .build()?;
        let endpoint = config.endpoint().as_ref().expect("endpoint should be set");
        assert_eq!(endpoint.timeout_ms, 1234);
        Ok(())
    }

    #[test]
    fn test_endpoint_with_api_key() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder()
            .endpoint_url("http://localhost:8126")
            .endpoint_api_key("my-api-key")
            .build()?;
        let endpoint = config.endpoint().as_ref().expect("endpoint should be set");
        assert_eq!(endpoint.api_key.as_deref(), Some("my-api-key"));
        Ok(())
    }

    #[test]
    fn test_endpoint_with_test_token() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder()
            .endpoint_url("http://localhost:8126")
            .endpoint_test_token("test-token")
            .build()?;
        let endpoint = config.endpoint().as_ref().expect("endpoint should be set");
        assert_eq!(endpoint.test_token.as_deref(), Some("test-token"));
        Ok(())
    }

    #[test]
    fn test_endpoint_with_system_resolver() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder()
            .endpoint_url("http://localhost:8126")
            .endpoint_use_system_resolver(true)
            .build()?;
        let endpoint = config.endpoint().as_ref().expect("endpoint should be set");
        assert!(endpoint.use_system_resolver);
        Ok(())
    }

    #[test]
    fn test_signals_default() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder().build()?;
        assert_eq!(config.signals(), &default_signals());
        Ok(())
    }

    #[test]
    fn test_signals_custom() -> anyhow::Result<()> {
        let signals = vec![libc::SIGSEGV, libc::SIGBUS];
        let config = CrashtrackerConfiguration::builder()
            .signals(signals.clone())
            .build()?;
        let mut expected = signals;
        expected.sort();
        assert_eq!(config.signals(), &expected);
        Ok(())
    }

    #[test]
    fn test_signals_duplicates_fail() {
        let result = CrashtrackerConfiguration::builder()
            .signals(vec![libc::SIGSEGV, libc::SIGSEGV])
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn test_signals_invalid_fail() {
        let result = CrashtrackerConfiguration::builder()
            .signals(vec![9999])
            .build();
        assert!(result.is_err());
    }

    #[test]
    fn test_timeout_default() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder().build()?;
        assert_eq!(config.timeout(), constants::DD_CRASHTRACK_DEFAULT_TIMEOUT);
        Ok(())
    }

    #[test]
    fn test_timeout_custom() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder()
            .timeout(Duration::from_secs(10))
            .build()?;
        assert_eq!(config.timeout(), Duration::from_secs(10));
        Ok(())
    }

    #[test]
    fn test_additional_files() -> anyhow::Result<()> {
        let files = vec!["/tmp/file1.txt".to_string(), "/tmp/file2.txt".to_string()];
        let config = CrashtrackerConfiguration::builder()
            .additional_files(files.clone())
            .build()?;
        assert_eq!(config.additional_files(), &files);
        Ok(())
    }

    #[test]
    fn test_demangle_names() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder()
            .demangle_names(true)
            .build()?;
        assert!(config.demangle_names());
        Ok(())
    }

    #[test]
    fn test_resolve_frames() -> anyhow::Result<()> {
        for variant in [
            StacktraceCollection::Disabled,
            StacktraceCollection::WithoutSymbols,
            StacktraceCollection::EnabledWithInprocessSymbols,
            StacktraceCollection::EnabledWithSymbolsInReceiver,
        ] {
            let config = CrashtrackerConfiguration::builder()
                .resolve_frames(variant)
                .build()?;
            assert_eq!(config.resolve_frames(), variant);
        }
        Ok(())
    }

    #[test]
    fn test_unix_socket_path() -> anyhow::Result<()> {
        let config = CrashtrackerConfiguration::builder()
            .unix_socket_path("/tmp/crashtracker.sock".to_string())
            .build()?;
        assert_eq!(
            config.unix_socket_path(),
            &Some("/tmp/crashtracker.sock".to_string())
        );
        Ok(())
    }
}
