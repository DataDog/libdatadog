// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Python wrapper for [`libdd_http_client::RetryConfig`].
//!
//! `RetryConfig` is opt-in: a freshly constructed [`crate::HttpClientBuilder`]
//! does not retry. Pass an instance to
//! [`crate::HttpClientBuilder::retry`] to enable retries.

use pyo3::prelude::*;
use std::time::Duration;

/// Python wrapper for [`libdd_http_client::RetryConfig`].
///
/// Mutable builder-style setters mirror the `HttpClientBuilder` shape used
/// elsewhere in this crate (chained mutations on a borrowed `&mut self`
/// rather than consuming `Self`, since pyo3 borrow rules forbid the latter).
#[pyclass(name = "RetryConfig", module = "libdd_http_client", from_py_object)]
#[derive(Clone)]
pub struct RetryConfig {
    inner: libdd_http_client::RetryConfig,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            inner: libdd_http_client::RetryConfig::new(),
        }
    }
}

#[pymethods]
impl RetryConfig {
    /// Construct a `RetryConfig` with the same defaults as
    /// [`libdd_http_client::RetryConfig::new`]: 3 retries, 100 ms initial
    /// delay, jitter enabled.
    #[new]
    #[pyo3(signature = (max_retries=None, initial_delay_millis=None, with_jitter=None))]
    pub fn new(
        max_retries: Option<u32>,
        initial_delay_millis: Option<u64>,
        with_jitter: Option<bool>,
    ) -> Self {
        let mut cfg = libdd_http_client::RetryConfig::new();
        if let Some(n) = max_retries {
            cfg = cfg.max_retries(n);
        }
        if let Some(ms) = initial_delay_millis {
            cfg = cfg.initial_delay(Duration::from_millis(ms));
        }
        if let Some(j) = with_jitter {
            cfg = cfg.with_jitter(j);
        }
        Self { inner: cfg }
    }

    /// Set the maximum number of retry attempts (not counting the initial
    /// request).
    fn set_max_retries(&mut self, n: u32) {
        // The Rust setter consumes `self`; we work around that by cloning the
        // current state into a new chain. Cheap because `RetryConfig` is plain
        // POD-ish data.
        let cfg = self.inner.clone().max_retries(n);
        self.inner = cfg;
    }

    /// Set the initial backoff delay in milliseconds. Subsequent retries
    /// double this value (exponential backoff).
    fn set_initial_delay_millis(&mut self, ms: u64) {
        let cfg = self.inner.clone().initial_delay(Duration::from_millis(ms));
        self.inner = cfg;
    }

    /// Enable or disable jitter.
    fn set_with_jitter(&mut self, jitter: bool) {
        let cfg = self.inner.clone().with_jitter(jitter);
        self.inner = cfg;
    }

    fn __repr__(&self) -> String {
        // `RetryConfig` does not expose getters, so we round-trip via Debug.
        format!("RetryConfig({:?})", self.inner)
    }
}

impl RetryConfig {
    /// Clone the inner [`libdd_http_client::RetryConfig`].
    pub(crate) fn to_inner(&self) -> libdd_http_client::RetryConfig {
        self.inner.clone()
    }
}
