// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Span link types for connecting inferred spans to upstream resources.

use sha2::{Digest, Sha256};

const SPAN_LINK_HASH_LENGTH: usize = 32;

/// A span link connecting an inferred span to an upstream resource
/// (e.g., an S3 object or a DynamoDB item).
#[derive(Debug, Clone, PartialEq)]
pub struct SpanLink {
    /// Deterministic hash identifying the resource.
    pub hash: String,
    /// Kind of span link (e.g., "aws.s3.object", "aws.dynamodb.item").
    pub kind: String,
}

/// Generates a deterministic hash from components joined by `|`.
///
/// Returns the first 32 hex characters of the SHA-256 digest.
/// See <https://github.com/DataDog/dd-span-pointer-rules/blob/main/README.md#General%20Hashing%20Rules>
#[must_use]
pub fn generate_span_link_hash(components: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(components.join("|").as_bytes());
    let result = hasher.finalize();
    hex::encode(result)[..SPAN_LINK_HASH_LENGTH].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_span_link_hash() {
        let test_cases = vec![
            (
                vec!["some-bucket", "some-key.data", "ab12ef34"],
                "e721375466d4116ab551213fdea08413",
            ),
            (
                vec!["some-bucket", "some-key.data", "ab12ef34-5"],
                "2b90dffc37ebc7bc610152c3dc72af9f",
            ),
        ];

        for (components, expected) in test_cases {
            assert_eq!(generate_span_link_hash(&components), expected);
        }
    }
}
