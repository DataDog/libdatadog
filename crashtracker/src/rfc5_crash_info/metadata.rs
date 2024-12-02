// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Metadata {
    pub library_name: String,
    pub library_version: String,
    pub family: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    /// A list of "key:value" tuples.
    pub tags: Vec<String>,
}

impl From<crate::crash_info::CrashtrackerMetadata> for Metadata {
    fn from(value: crate::crash_info::CrashtrackerMetadata) -> Self {
        let tags = value
            .tags
            .into_iter()
            .map(|t: ddcommon::tag::Tag| t.to_string())
            .collect();
        Self {
            library_name: value.library_name,
            library_version: value.library_version,
            family: value.family,
            tags,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Metadata;
    use crate::rfc5_crash_info::test_utils::TestInstance;

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
