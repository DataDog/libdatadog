// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::metric::{Metric, SortedTags};
use datadog_protos::metrics::Origin;

// Metric tag keys
const DD_ORIGIN_TAG_KEY: &str = "origin";
const AWS_LAMBDA_TAG_KEY: &str = "function_arn";
const AWS_STEP_FUNCTIONS_TAG_KEY: &str = "statemachinearn";

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
const AWS_STEP_FUNCTIONS_PREFIX: &str = "aws.states";

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
    StepFunctionsMetrics = 41,
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

pub fn find_metric_origin(metric: &Metric, tags: SortedTags) -> Option<Origin> {
    let name = metric.name.to_string();
    let prefix = name.split('.').take(2).collect::<Vec<&str>>().join(".");

    if is_datadog_metric(&prefix) {
        return None;
    }
    if is_azure_app_services(&tags, &prefix) {
        return Some(Origin {
            origin_product: OriginProduct::Serverless.into(),
            origin_category: OriginCategory::AppServicesMetrics.into(),
            origin_service: OriginService::Other.into(),
            ..Default::default()
        });
    }
    if is_google_cloud_run(&tags, &prefix) {
        return Some(Origin {
            origin_product: OriginProduct::Serverless.into(),
            origin_category: OriginCategory::CloudRunMetrics.into(),
            origin_service: OriginService::Other.into(),
            ..Default::default()
        });
    }
    if is_azure_container_app(&tags, &prefix) {
        return Some(Origin {
            origin_product: OriginProduct::Serverless.into(),
            origin_category: OriginCategory::ContainerAppMetrics.into(),
            origin_service: OriginService::Other.into(),
            ..Default::default()
        });
    }
    if is_azure_functions(&tags, &prefix) {
        return Some(Origin {
            origin_product: OriginProduct::Serverless.into(),
            origin_category: OriginCategory::AzureFunctionsMetrics.into(),
            origin_service: OriginService::Other.into(),
            ..Default::default()
        });
    }
    if is_aws_lambda(&tags, &prefix) {
        return Some(Origin {
            origin_product: OriginProduct::Serverless.into(),
            origin_category: OriginCategory::LambdaMetrics.into(),
            origin_service: OriginService::Other.into(),
            ..Default::default()
        });
    }
    if is_aws_step_functions(&tags, &prefix) {
        return Some(Origin {
            origin_product: OriginProduct::Serverless.into(),
            origin_category: OriginCategory::StepFunctionsMetrics.into(),
            origin_service: OriginService::Other.into(),
            ..Default::default()
        });
    }

    None
}

fn get_first_tag_value<'a>(tags: &'a SortedTags, key: &str) -> Option<&'a str> {
    tags.find_all(key)
        .iter()
        .filter_map(|value| {
            if !value.is_empty() {
                Some(value.as_str())
            } else {
                None
            }
        })
        .next()
}

fn is_datadog_metric(prefix: &str) -> bool {
    prefix == DATADOG_PREFIX
}

fn is_google_cloud_run(tags: &SortedTags, prefix: &str) -> bool {
    get_first_tag_value(tags, DD_ORIGIN_TAG_KEY) == Some(GOOGLE_CLOUD_RUN_TAG_VALUE)
        && prefix != GOOGLE_CLOUD_RUN_PREFIX
}

fn is_azure_app_services(tags: &SortedTags, prefix: &str) -> bool {
    get_first_tag_value(tags, DD_ORIGIN_TAG_KEY) == Some(AZURE_APP_SERVICES_TAG_VALUE)
        && prefix != AZURE_APP_SERVICES_PREFIX
}

fn is_azure_container_app(tags: &SortedTags, prefix: &str) -> bool {
    get_first_tag_value(tags, DD_ORIGIN_TAG_KEY) == Some(AZURE_CONTAINER_APP_TAG_VALUE)
        && prefix != AZURE_CONTAINER_APP_PREFIX
}

fn is_azure_functions(tags: &SortedTags, prefix: &str) -> bool {
    get_first_tag_value(tags, DD_ORIGIN_TAG_KEY) == Some(AZURE_FUNCTIONS_TAG_VALUE)
        && prefix != AZURE_FUNCTIONS_PREFIX
}

fn is_aws_lambda(tags: &SortedTags, prefix: &str) -> bool {
    get_first_tag_value(tags, AWS_LAMBDA_TAG_KEY).is_some() && prefix != AWS_LAMBDA_PREFIX
}

fn is_aws_step_functions(tags: &SortedTags, prefix: &str) -> bool {
    get_first_tag_value(tags, AWS_STEP_FUNCTIONS_TAG_KEY).is_some()
        && prefix != AWS_STEP_FUNCTIONS_PREFIX
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
        let metric = Metric {
            id: 0,
            name: "aws.lambda.enhanced.invocations".into(),
            value: MetricValue::Gauge(1.0),
            tags: Some(tags.clone()),
        };
        let origin = find_metric_origin(&metric, tags);
        assert_eq!(origin, None);
    }

    #[test]
    fn test_find_metric_origin_aws_lambda_custom_metric() {
        let tags = SortedTags::parse("function_arn:hello123").unwrap();
        let metric = Metric {
            id: 0,
            name: "my.custom.aws.lambda.invocations".into(),
            value: MetricValue::Gauge(1.0),
            tags: Some(tags.clone()),
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
    fn test_get_first_tag_value() {
        let tags = SortedTags::parse("a,a:1,b:2,c:3").unwrap();
        assert_eq!(get_first_tag_value(&tags, "a"), Some("1"));
        assert_eq!(get_first_tag_value(&tags, "b"), Some("2"));
        assert_eq!(get_first_tag_value(&tags, "c"), Some("3"));
        assert_eq!(get_first_tag_value(&tags, "d"), None);
    }
}
