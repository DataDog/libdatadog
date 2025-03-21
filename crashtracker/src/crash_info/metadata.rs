// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#[cfg(feature = "pyo3")]
use pyo3::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::unknown_value::UnknownValue;

#[cfg_attr(feature = "pyo3", pyclass)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Metadata {
    pub library_name: String,
    pub library_version: String,
    pub family: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    /// A list of "key:value" tuples.
    pub tags: Vec<String>,
}

impl Metadata {
    fn new_internal(
        library_name: String,
        library_version: String,
        family: String,
        tags: Vec<String>,
    ) -> Self {
        Self {
            library_name,
            library_version,
            family,
            tags,
        }
    }

    #[cfg(not(feature = "pyo3"))]
    pub fn new(
        library_name: String,
        library_version: String,
        family: String,
        tags: Vec<String>,
    ) -> Self {
        Self::new_internal(library_name, library_version, family, tags)
    }
}

#[cfg_attr(feature = "pyo3", pymethods)]
impl Metadata {
    #[cfg(feature = "pyo3")]
    #[new]
    #[pyo3(signature = (library_name, library_version, family, tags))]
    pub fn new(
        library_name: String,
        library_version: String,
        family: String,
        tags: Vec<String>,
    ) -> Self {
        Self::new_internal(library_name, library_version, family, tags)
    }
}

impl UnknownValue for Metadata {
    fn unknown_value() -> Self {
        Self {
            library_name: "unknown".to_string(),
            library_version: "unknown".to_string(),
            family: "unknown".to_string(),
            tags: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Metadata;
    use crate::crash_info::test_utils::TestInstance;

    macro_rules! tag {
        ($key:expr, $val:expr) => {
            format!("{}:{}", $key, $val)
        };
    }

    impl TestInstance for Metadata {
        fn test_instance(seed: u64) -> Self {
            Self {
                library_name: "libdatadog".to_owned(),
                library_version: format!("{}.{}.{}", seed, seed + 1, seed + 2),
                family: "native".to_owned(),
                tags: vec![
                    tag!("service", "foo"),
                    tag!("service_version", "bar"),
                    tag!("runtime-id", "xyz"),
                    tag!("language", "native"),
                ],
            }
        }
    }
}
