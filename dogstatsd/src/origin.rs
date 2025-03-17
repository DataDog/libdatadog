// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::metric::{Metric, SortedTags};
use datadog_protos::metrics::Origin;

// Metric tag keys
const DD_ORIGIN_TAG_KEY: &str = "origin";
const AWS_LAMBDA_TAG_KEY: &str = "function_arn";

// Metric tag values
const GOOGLE_CLOUD_RUN_TAG_VALUE: &str = "cloudrun";
const AZURE_APP_SERVICES_TAG_VALUE: &str = "appservice";
const AZURE_CONTAINER_APP_TAG_VALUE: &str = "containerapp";
const AZURE_FUNCTIONS_TAG_VALUE: &str = "azurefunction";

// Metric prefixes
const DATADOG_PREFIX: &str = "datadog";
const GOOGLE_CLOUD_RUN_PREFIX: &str = "gcp.run";
const AZURE_APP_SERVICES_PREFIX: &str = "azure.app_services";
const AZURE_CONTAINER_APP_PREFIX: &str = "azure.app_containerapps";
const AZURE_FUNCTIONS_PREFIX: &str = "azure.functions";
const AWS_LAMBDA_PREFIX: &str = "aws.lambda";

/// Represents the product origin of a metric.
/// The full enum is exhaustive so we only include what we need. Please reference the corresponding
/// enum for all possible values https://github.com/DataDog/dd-source/blob/573dee9b5f7ee13935cb3ad11b16dde970528983/domains/metrics/shared/libs/proto/origin/origin.proto#L161
pub enum OriginProduct {
    Other = 0,
    Serverless = 1,
}

impl From<OriginProduct> for u32 {
    fn from(product: OriginProduct) -> u32 {
        product as u32
    }
}

/// Represents the category origin of a metric.
/// The full enum is exhaustive so we only include what we need. Please reference the corresponding
/// enum for all possible values https://github.com/DataDog/dd-source/blob/573dee9b5f7ee13935cb3ad11b16dde970528983/domains/metrics/shared/libs/proto/origin/origin.proto#L276
pub enum OriginCategory {
    Other = 0,
    AppServicesMetrics = 35,
    CloudRunMetrics = 36,
    ContainerAppMetrics = 37,
    LambdaMetrics = 38,
    AzureFunctionsMetrics = 71,
}

impl From<OriginCategory> for u32 {
    fn from(category: OriginCategory) -> u32 {
        category as u32
    }
}

/// Represents the service origin of a metric.
/// The full enum is exhaustive so we only include what we need. Please reference the corresponding
/// enum for all possible values https://github.com/DataDog/dd-source/blob/573dee9b5f7ee13935cb3ad11b16dde970528983/domains/metrics/shared/libs/proto/origin/origin.proto#L417
pub enum OriginService {
    Other = 0,
}

impl From<OriginService> for u32 {
    fn from(service: OriginService) -> u32 {
        service as u32
    }
}

/// Struct to hold tag key, tag value, and prefix for matching.
struct MetricOriginCheck {
    tag_key: &'static str,
    tag_value: &'static str,
    prefix: &'static str,
}

impl MetricOriginCheck {
    /// Checks if the tag matches the given key, value, and prefix.
    fn matches(&self, tags: &SortedTags, metric_prefix: &str) -> bool {
        has_tag_value(tags, self.tag_key, self.tag_value) && metric_prefix != self.prefix
    }
}

const METRIC_ORIGIN_CHECKS: &[MetricOriginCheck] = &[
    MetricOriginCheck {
        tag_key: DD_ORIGIN_TAG_KEY,
        tag_value: GOOGLE_CLOUD_RUN_TAG_VALUE,
        prefix: GOOGLE_CLOUD_RUN_PREFIX,
    },
    MetricOriginCheck {
        tag_key: DD_ORIGIN_TAG_KEY,
        tag_value: AZURE_APP_SERVICES_TAG_VALUE,
        prefix: AZURE_APP_SERVICES_PREFIX,
    },
    MetricOriginCheck {
        tag_key: DD_ORIGIN_TAG_KEY,
        tag_value: AZURE_CONTAINER_APP_TAG_VALUE,
        prefix: AZURE_CONTAINER_APP_PREFIX,
    },
    MetricOriginCheck {
        tag_key: DD_ORIGIN_TAG_KEY,
        tag_value: AZURE_FUNCTIONS_TAG_VALUE,
        prefix: AZURE_FUNCTIONS_PREFIX,
    },
    MetricOriginCheck {
        tag_key: AWS_LAMBDA_TAG_KEY,
        tag_value: "",
        prefix: AWS_LAMBDA_PREFIX,
    },
];

/// Creates an Origin for serverless metrics.
fn serverless_origin(category: OriginCategory) -> Origin {
    Origin {
        origin_product: OriginProduct::Serverless.into(),
        origin_service: OriginService::Other.into(),
        origin_category: category.into(),
        ..Default::default()
    }
}

/// Finds the origin of a metric based on its tags and name prefix.
pub fn find_metric_origin(metric: &Metric, tags: SortedTags) -> Option<Origin> {
    let metric_name = metric.name.to_string();
    let metric_prefix = metric_name
        .split('.')
        .take(2)
        .collect::<Vec<&str>>()
        .join(".");

    if is_datadog_metric(&metric_prefix) {
        return None;
    }

    for (index, origin_check) in METRIC_ORIGIN_CHECKS.iter().enumerate() {
        if origin_check.matches(&tags, &metric_prefix) {
            let category = match index {
                0 => OriginCategory::CloudRunMetrics,
                1 => OriginCategory::AppServicesMetrics,
                2 => OriginCategory::ContainerAppMetrics,
                3 => OriginCategory::AzureFunctionsMetrics,
                4 => OriginCategory::LambdaMetrics,
                _ => OriginCategory::Other,
            };
            return Some(serverless_origin(category));
        }
    }

    None
}

/// Checks if the given key-value pair exists in the tags.
fn has_tag_value(tags: &SortedTags, key: &str, value: &str) -> bool {
    if value.is_empty() {
        return !tags.find_all(key).is_empty();
    }
    tags.find_all(key)
        .iter()
        .any(|tag_value| tag_value.as_str() == value)
}

/// Checks if the metric is a Datadog metric.
fn is_datadog_metric(prefix: &str) -> bool {
    prefix == DATADOG_PREFIX
}

#[cfg(test)]
mod tests {
    use crate::metric::MetricValue;

    use super::*;

    #[test]
    fn test_origin_product() {
        let origin_product: u32 = OriginProduct::Serverless.into();
        assert_eq!(origin_product, 1);
    }

    #[test]
    fn test_origin_category() {
        let origin_category: u32 = OriginCategory::LambdaMetrics.into();
        assert_eq!(origin_category, 38);
    }

    #[test]
    fn test_origin_service() {
        let origin_service: u32 = OriginService::Other.into();
        assert_eq!(origin_service, 0);
    }

    #[test]
    fn test_find_metric_origin_aws_lambda_standard_metric() {
        let tags = SortedTags::parse("function_arn:hello123").unwrap();
        let mut now = 1656581409;
        now = (now / 10) * 10;

        let metric = Metric {
            id: 0,
            name: "aws.lambda.enhanced.invocations".into(),
            value: MetricValue::Gauge(1.0),
            tags: Some(tags.clone()),
            timestamp: now,
        };
        let origin = find_metric_origin(&metric, tags);
        assert_eq!(origin, None);
    }

    #[test]
    fn test_find_metric_origin_aws_lambda_custom_metric() {
        let tags = SortedTags::parse("function_arn:hello123").unwrap();
        let mut now = std::time::UNIX_EPOCH
            .elapsed()
            .expect("unable to poll clock, unrecoverable")
            .as_secs()
            .try_into()
            .unwrap_or_default();
        now = (now / 10) * 10;

        let metric = Metric {
            id: 0,
            name: "my.custom.aws.lambda.invocations".into(),
            value: MetricValue::Gauge(1.0),
            tags: Some(tags.clone()),
            timestamp: now,
        };
        let origin = find_metric_origin(&metric, tags);
        assert_eq!(
            origin,
            Some(Origin {
                origin_product: OriginProduct::Serverless.into(),
                origin_category: OriginCategory::LambdaMetrics.into(),
                origin_service: OriginService::Other.into(),
                ..Default::default()
            })
        );
    }

    #[test]
    fn test_has_tag_value() {
        let tags = SortedTags::parse("a,a:1,b:2,c:3").unwrap();
        assert!(has_tag_value(&tags, "a", "1"));
        assert!(has_tag_value(&tags, "b", "2"));
        assert!(has_tag_value(&tags, "c", "3"));
        assert!(!has_tag_value(&tags, "d", "4"));
    }
}
