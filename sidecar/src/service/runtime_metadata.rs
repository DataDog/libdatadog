// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

/// `RuntimeMetadata` is a struct that represents the runtime metadata of a language.
#[derive(Default, Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeMetadata {
    pub language_name: String,
    pub language_version: String,
    pub tracer_version: String,
}

impl RuntimeMetadata {
    /// Creates a new `RuntimeMetadata` instance.
    ///
    /// This method takes three parameters: `language_name`, `language_version`, and
    /// `tracer_version`, and returns a new `RuntimeMetadata` instance.
    ///
    /// # Parameters
    ///
    /// * `language_name`: The name of the language.
    /// * `language_version`: The version of the language.
    /// * `tracer_version`: The version of the tracer.
    ///
    /// # Examples
    ///
    /// ```
    /// use datadog_sidecar::service::RuntimeMetadata;
    ///
    /// let metadata = RuntimeMetadata::new("Rust", "1.55.0", "0.1.0");
    /// ```
    pub fn new<T>(language_name: T, language_version: T, tracer_version: T) -> Self
    where
        T: Into<String>,
    {
        Self {
            language_name: language_name.into(),
            language_version: language_version.into(),
            tracer_version: tracer_version.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let language_name = "Rust";
        let language_version = "1.55.0";
        let tracer_version = "0.1.0";

        let metadata = RuntimeMetadata::new(language_name, language_version, tracer_version);

        assert_eq!(metadata.language_name, language_name);
        assert_eq!(metadata.language_version, language_version);
        assert_eq!(metadata.tracer_version, tracer_version);
    }
}
