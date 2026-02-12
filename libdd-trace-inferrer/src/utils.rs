// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared utility functions.

use regex::Regex;
use std::sync::OnceLock;

/// Milliseconds to nanoseconds.
pub const MS_TO_NS: f64 = 1_000_000.0;

/// Seconds to nanoseconds.
pub const S_TO_NS: f64 = 1_000_000_000.0;

fn ulid_uuid_guid_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // SAFETY: This regex pattern is a compile-time constant and always valid.
        #[allow(clippy::expect_used)]
        Regex::new(
            r"(?x)
            (
                [0-9a-fA-F]{8}-          # UUID/GUID segment 1
                [0-9a-fA-F]{4}-          # segment 2
                [0-9a-fA-F]{4}-          # segment 3
                [0-9a-fA-F]{4}-          # segment 4
                [0-9a-fA-F]{12}          # segment 5
            )
            |
            (
                [0123456789ABCDEFGHJKMNPQRSTVWXYZ]{26}  # ULID
            )
        ",
        )
        .expect("ULID/UUID/GUID regex is a compile-time constant and always valid")
    })
}

/// Parameterizes URL path segments that look like identifiers (numeric,
/// UUID, ULID) by replacing them with `{param_id}` placeholders.
///
/// If the resource already contains curly braces (API Gateway parameters),
/// it is returned unchanged.
#[must_use]
pub fn parameterize_api_resource(resource: String) -> String {
    if resource.contains('{') && resource.contains('}') {
        return resource;
    }

    let parts: Vec<&str> = resource.split('/').collect();
    let mut result = Vec::new();
    result.push(String::new());

    for (i, part) in parts.iter().enumerate().skip(1) {
        if part.is_empty() {
            continue;
        }

        if part.chars().all(|c| c.is_ascii_digit()) || ulid_uuid_guid_regex().is_match(part) {
            let param_name = if i > 1 && !parts[i - 1].is_empty() {
                let singular = parts[i - 1].trim_end_matches('s');
                if singular == "id" {
                    singular.into()
                } else {
                    format!("{singular}_id")
                }
            } else {
                "id".to_string()
            };
            result.push(format!("{{{param_name}}}"));
        } else {
            result.push((*part).to_string());
        }
    }
    result.join("/")
}

/// Returns the AWS partition string for a given region.
#[must_use]
pub fn get_aws_partition_by_region(region: &str) -> &'static str {
    match region {
        r if r.starts_with("us-gov-") => "aws-us-gov",
        r if r.starts_with("cn-") => "aws-cn",
        _ => "aws",
    }
}

/// Returns a default service name based on whether instance-level naming is
/// enabled.
#[must_use]
pub fn get_default_service_name(
    instance_name: &str,
    fallback: &str,
    use_instance_names: bool,
) -> String {
    if use_instance_names {
        instance_name.to_string()
    } else {
        fallback.to_string()
    }
}

/// Resolves a service name using service mapping configuration.
///
/// Priority: specific identifier -> generic identifier -> default name.
#[must_use]
pub fn resolve_service_name(
    service_mapping: &std::collections::HashMap<String, String>,
    specific_id: &str,
    generic_id: &str,
    instance_name: &str,
    fallback: &str,
    use_instance_names: bool,
) -> String {
    service_mapping
        .get(specific_id)
        .or_else(|| service_mapping.get(generic_id))
        .cloned()
        .unwrap_or_else(|| get_default_service_name(instance_name, fallback, use_instance_names))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_parameterize_numeric() {
        assert_eq!(
            parameterize_api_resource("/users/12345/profile".to_string()),
            "/users/{user_id}/profile"
        );
    }

    #[test]
    fn test_parameterize_uuid() {
        assert_eq!(
            parameterize_api_resource(
                "/items/550e8400-e29b-41d4-a716-446655440000".to_string()
            ),
            "/items/{item_id}"
        );
    }

    #[test]
    fn test_parameterize_already_parameterized() {
        let resource = "/users/{id}/profile".to_string();
        assert_eq!(parameterize_api_resource(resource.clone()), resource);
    }

    #[test]
    fn test_get_aws_partition() {
        assert_eq!(get_aws_partition_by_region("us-east-1"), "aws");
        assert_eq!(get_aws_partition_by_region("us-gov-west-1"), "aws-us-gov");
        assert_eq!(get_aws_partition_by_region("cn-north-1"), "aws-cn");
    }

    #[test]
    fn test_resolve_service_name_priority() {
        let mapping = HashMap::from([
            ("my-queue".to_string(), "specific-svc".to_string()),
            ("lambda_sqs".to_string(), "generic-svc".to_string()),
        ]);

        // Specific takes priority
        assert_eq!(
            resolve_service_name(&mapping, "my-queue", "lambda_sqs", "my-queue", "sqs", true),
            "specific-svc"
        );

        // Generic when no specific
        assert_eq!(
            resolve_service_name(&mapping, "other-queue", "lambda_sqs", "other-queue", "sqs", true),
            "generic-svc"
        );

        // Instance name when no mapping and use_instance_names=true
        let empty = HashMap::new();
        assert_eq!(
            resolve_service_name(&empty, "other-queue", "lambda_sqs", "other-queue", "sqs", true),
            "other-queue"
        );

        // Fallback when no mapping and use_instance_names=false
        assert_eq!(
            resolve_service_name(&empty, "other-queue", "lambda_sqs", "other-queue", "sqs", false),
            "sqs"
        );
    }
}
