// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Language/runtime metadata injected into every outgoing request.

/// Language and runtime metadata that is automatically injected into every request as
/// `Datadog-Meta-*` headers and drives the `User-Agent` string.
#[derive(Debug, Clone)]
pub struct LanguageMetadata {
    /// Value of `Datadog-Meta-Lang`, e.g. `"python"`.
    pub language: String,
    /// Value of `Datadog-Meta-Lang-Version`, e.g. `"3.12.1"`.
    pub language_version: String,
    /// Value of `Datadog-Meta-Lang-Interpreter`, e.g. `"CPython"`.
    pub interpreter: String,
    /// Value of `Datadog-Meta-Tracer-Version`, e.g. `"2.18.0"`.
    pub tracer_version: String,
}

impl LanguageMetadata {
    /// Construct a new `LanguageMetadata`.
    pub fn new(
        language: impl Into<String>,
        language_version: impl Into<String>,
        interpreter: impl Into<String>,
        tracer_version: impl Into<String>,
    ) -> Self {
        todo!()
    }

    /// Produces the `User-Agent` string passed to `Endpoint::to_request_builder()`.
    ///
    /// Format: `dd-trace-<language>/<tracer_version>`, e.g. `dd-trace-python/2.18.0`.
    pub fn user_agent(&self) -> String {
        todo!()
    }
}
