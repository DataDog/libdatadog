// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Configuration for span inference.

use std::collections::HashMap;

/// Configuration for the span inferrer.
///
/// This replaces the bottlecap `Config` + `AwsConfig` with a focused,
/// non-Lambda-specific configuration struct.
#[derive(Debug, Clone)]
pub struct InferConfig {
    /// Service mapping: maps trigger-specific identifiers or generic keys
    /// (e.g., "lambda_sqs", "lambda_api_gateway") to custom service names.
    pub service_mapping: HashMap<String, String>,

    /// When true, use the AWS resource instance name (e.g., queue name, domain
    /// name) as the default service name. When false, use a generic fallback
    /// (e.g., "sqs", "apigateway").
    ///
    /// Corresponds to bottlecap's `trace_aws_service_representation_enabled`.
    pub use_instance_service_names: bool,

    /// AWS region, used for ARN construction and partition detection.
    pub region: String,
}

impl Default for InferConfig {
    fn default() -> Self {
        Self {
            service_mapping: HashMap::new(),
            use_instance_service_names: true,
            region: String::new(),
        }
    }
}
