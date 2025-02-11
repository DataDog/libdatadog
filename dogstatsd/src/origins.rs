// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::metric::Metric;
use datadog_protos::metrics::{Metadata, Origin};
use protobuf::MessageField;

const AZURE_APP_SERVICES_PREFIX: &str = "azure.app_services";
const GOOGLE_CLOUD_RUN_PREFIX: &str = "gcp.run";
const AZURE_CONTAINER_APP_PREFIX: &str = "azure.app_containerapps";
const AWS_LAMBDA_PREFIX: &str = "aws.lambda";
const AWS_STEP_FUNCTIONS_PREFIX: &str = "aws.states";

/// Represents the product origin of a metric.
/// The full enum is exhaustive so we only include what we need. Please reference the corresponding enum for all possible values
/// https://github.com/DataDog/dd-source/blob/573dee9b5f7ee13935cb3ad11b16dde970528983/domains/metrics/shared/libs/proto/origin/origin.proto#L161
pub enum OriginProduct {
    Serverless = 1,
}

impl From<OriginProduct> for u32 {
    fn from(product: OriginProduct) -> u32 {
        product as u32
    }
}

/// Represents the category origin of a metric.
/// The full enum is exhaustive so we only include what we need. Please reference the corresponding enum for all possible values
/// https://github.com/DataDog/dd-source/blob/573dee9b5f7ee13935cb3ad11b16dde970528983/domains/metrics/shared/libs/proto/origin/origin.proto#L276
pub enum OriginCategory {
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
/// The full enum is exhaustive so we only include what we need. Please reference the corresponding enum for all possible values
/// https://github.com/DataDog/dd-source/blob/573dee9b5f7ee13935cb3ad11b16dde970528983/domains/metrics/shared/libs/proto/origin/origin.proto#L417
pub enum OriginService {
    Other = 0,
}

impl From<OriginService> for u32 {
    fn from(service: OriginService) -> u32 {
        service as u32
    }
}

pub fn get_origin(metric: &Metric) -> Option<Metadata> {
    println!("========================== Metric: {:?}", metric);
    let name = metric.name.to_string();
    let prefix = name.split('.').take(2).collect::<Vec<&str>>().join(".");

    if let Some(tags) = &metric.tags {
        println!("========================== Metric tags: {:?}", tags);
        if tags.contains("env") {
            println!("======================== FOUND TAG ========================");
        }
    }

    match prefix {
        _ if prefix == AZURE_APP_SERVICES_PREFIX => Some(Metadata {
            origin: MessageField::some(Origin {
                origin_product: OriginProduct::Serverless.into(),
                origin_category: OriginCategory::AppServicesMetrics.into(),
                origin_service: OriginService::Other.into(),
                special_fields: Default::default(),
            }),
            ..Default::default()
        }),
        _ if prefix == GOOGLE_CLOUD_RUN_PREFIX => Some(Metadata {
            origin: MessageField::some(Origin {
                origin_product: OriginProduct::Serverless.into(),
                origin_category: OriginCategory::CloudRunMetrics.into(),
                origin_service: OriginService::Other.into(),
                special_fields: Default::default(),
            }),
            ..Default::default()
        }),
        _ if prefix == AZURE_CONTAINER_APP_PREFIX => Some(Metadata {
            origin: MessageField::some(Origin {
                origin_product: OriginProduct::Serverless.into(),
                origin_category: OriginCategory::ContainerAppMetrics.into(),
                origin_service: OriginService::Other.into(),
                special_fields: Default::default(),
            }),
            ..Default::default()
        }),
        _ if prefix == AWS_LAMBDA_PREFIX => Some(Metadata {
            origin: MessageField::some(Origin {
                origin_product: OriginProduct::Serverless.into(),
                origin_category: OriginCategory::LambdaMetrics.into(),
                origin_service: OriginService::Other.into(),
                special_fields: Default::default(),
            }),
            ..Default::default()
        }),
        _ if prefix == AWS_STEP_FUNCTIONS_PREFIX => Some(Metadata {
            origin: MessageField::some(Origin {
                origin_product: OriginProduct::Serverless.into(),
                origin_category: OriginCategory::StepFunctionsMetrics.into(),
                origin_service: OriginService::Other.into(),
                special_fields: Default::default(),
            }),
            ..Default::default()
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
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
    fn test_get_origin() {
        let origin = get_origin("aws.lambda.enhanced.invocations");
        assert_eq!(
            origin
                .as_ref()
                .unwrap()
                .origin
                .as_ref()
                .unwrap()
                .origin_product,
            1
        );
        assert_eq!(
            origin
                .as_ref()
                .unwrap()
                .origin
                .as_ref()
                .unwrap()
                .origin_category,
            38
        );
        assert_eq!(
            origin
                .as_ref()
                .unwrap()
                .origin
                .as_ref()
                .unwrap()
                .origin_service,
            0
        );
    }
}
