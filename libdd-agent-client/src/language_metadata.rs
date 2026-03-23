// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Language/runtime metadata injected into every outgoing request.

/// Language and runtime metadata that is automatically injected into every request as
/// `Datadog-Meta-*` headers and drives the `User-Agent` string.
///
/// | Header                          | Field              |
/// |---------------------------------|--------------------|
/// | `Datadog-Meta-Lang`             | `language`         |
/// | `Datadog-Meta-Lang-Version`     | `language_version` |
/// | `Datadog-Meta-Lang-Interpreter` | `interpreter`      |
/// | `Datadog-Meta-Tracer-Version`   | `tracer_version`   |
///
/// These four headers are today manually assembled in four separate places in dd-trace-py:
/// `writer.py:638-644`, `writer.py:785-792`, `stats.py:113-117`, and
/// `datastreams/processor.py:128-133`. A single `LanguageMetadata` instance replaces all of them.
///
/// # `User-Agent`
///
/// [`LanguageMetadata::user_agent`] produces the string passed to
/// `Endpoint::to_request_builder(user_agent)`, so the `User-Agent` and the `Datadog-Meta-*`
/// headers share a single source of truth.
#[derive(Debug, Clone)]
pub struct LanguageMetadata {
    /// Language name, e.g. `"python"`.
    pub language: String,
    /// Language runtime version, e.g. `"3.12.1"`.
    pub language_version: String,
    /// Interpreter name, e.g. `"CPython"`.
    pub interpreter: String,
    /// Tracer library version, e.g. `"2.18.0"`.
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
