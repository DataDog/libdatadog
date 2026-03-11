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
