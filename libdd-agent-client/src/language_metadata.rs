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
        Self {
            language: language.into(),
            language_version: language_version.into(),
            interpreter: interpreter.into(),
            tracer_version: tracer_version.into(),
        }
    }

    /// Produces the `User-Agent` header.
    ///
    /// Format: `dd-trace-<language>/<tracer_version>`, e.g. `dd-trace-python/2.18.0`.
    #[inline]
    pub(crate) fn user_agent(&self) -> String {
        format!("dd-trace-{}/{}", self.language, self.tracer_version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_stores_fields() {
        let m = LanguageMetadata::new("python", "3.12.1", "CPython", "2.18.0");
        assert_eq!(m.language, "python");
        assert_eq!(m.language_version, "3.12.1");
        assert_eq!(m.interpreter, "CPython");
        assert_eq!(m.tracer_version, "2.18.0");
    }

    #[test]
    fn user_agent_format() {
        let m = LanguageMetadata::new("python", "3.12.1", "CPython", "2.18.0");
        assert_eq!(m.user_agent(), "dd-trace-python/2.18.0");
    }

    #[test]
    fn user_agent_ruby() {
        let m = LanguageMetadata::new("ruby", "3.2.0", "MRI", "1.13.0");
        assert_eq!(m.user_agent(), "dd-trace-ruby/1.13.0");
    }
}
