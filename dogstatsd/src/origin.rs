// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::metric::{Metric, SortedTags};
use datadog_protos::metrics::Origin;

const DD_ORIGIN_TAG_KEY: &str = "origin";
const AWS_LAMBDA_TAG_KEY: &str = "function_arn";
const AWS_STEP_FUNCTIONS_TAG_KEY: &str = "statemachinearn";

const AZURE_APP_SERVICES_TAG_VALUE: &str = "appservice";
const GOOGLE_CLOUD_RUN_TAG_VALUE: &str = "cloudrun";
const AZURE_CONTAINER_APP_TAG_VALUE: &str = "containerapp";

const DATADOG_PREFIX: &str = "datadog";
const AZURE_APP_SERVICES_PREFIX: &str = "azure.app_services";
const GOOGLE_CLOUD_RUN_PREFIX: &str = "gcp.run";
const AZURE_CONTAINER_APP_PREFIX: &str = "azure.app_containerapps";
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

pub fn get_origin(metric: &Metric, tags: SortedTags) -> Option<Origin> {
    let name = metric.name.to_string();
    let prefix = name.split('.').take(2).collect::<Vec<&str>>().join(".");

    let origin: Option<Origin> = match tags.get(DD_ORIGIN_TAG_KEY) {
        Some(AZURE_APP_SERVICES_TAG_VALUE) if prefix != AZURE_APP_SERVICES_PREFIX => Some(Origin {
            origin_product: OriginProduct::Serverless.into(),
            origin_category: OriginCategory::AppServicesMetrics.into(),
            origin_service: OriginService::Other.into(),
            ..Default::default()
        }),
        Some(GOOGLE_CLOUD_RUN_TAG_VALUE) if prefix != GOOGLE_CLOUD_RUN_PREFIX => Some(Origin {
            origin_product: OriginProduct::Serverless.into(),
            origin_category: OriginCategory::CloudRunMetrics.into(),
            origin_service: OriginService::Other.into(),
            ..Default::default()
        }),
        Some(AZURE_CONTAINER_APP_TAG_VALUE) if prefix != AZURE_CONTAINER_APP_PREFIX => {
            Some(Origin {
                origin_product: OriginProduct::Serverless.into(),
                origin_category: OriginCategory::ContainerAppMetrics.into(),
                origin_service: OriginService::Other.into(),
                ..Default::default()
            })
        }
        _ if tags.contains(AWS_LAMBDA_TAG_KEY) && prefix != AWS_LAMBDA_PREFIX => Some(Origin {
            origin_product: OriginProduct::Serverless.into(),
            origin_category: OriginCategory::LambdaMetrics.into(),
            origin_service: OriginService::Other.into(),
            ..Default::default()
        }),
        _ if tags.contains(AWS_STEP_FUNCTIONS_TAG_KEY) && prefix != AWS_STEP_FUNCTIONS_PREFIX => {
            Some(Origin {
                origin_product: OriginProduct::Serverless.into(),
                origin_category: OriginCategory::StepFunctionsMetrics.into(),
                origin_service: OriginService::Other.into(),
                ..Default::default()
            })
        }
        _ if prefix == DATADOG_PREFIX => return None,
        _ => return None,
    };
    origin
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
    fn test_get_origin_aws_lambda_standard_metric() {
        let tags = SortedTags::parse("function_arn:hello123").unwrap();
        let metric = Metric {
            id: 0,
            name: "aws.lambda.enhanced.invocations".into(),
            value: MetricValue::Gauge(1.0),
            tags: Some(tags.clone()),
        };
        let origin = get_origin(&metric, tags);
        assert_eq!(origin, None);
    }

    #[test]
    fn test_get_origin_aws_lambda_custom_metric() {
        let tags = SortedTags::parse("function_arn:hello123").unwrap();
        let metric = Metric {
            id: 0,
            name: "my.custom.aws.lambda.invocations".into(),
            value: MetricValue::Gauge(1.0),
            tags: Some(tags.clone()),
        };
        let origin = get_origin(&metric, tags);
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
}
