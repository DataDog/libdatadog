// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::trace_utils;
use std::env;

pub const PROD_INTAKE_SUBDOMAIN: &str = "trace.agent";

const TRACE_INTAKE_ROUTE: &str = "/api/v0.2/traces";
const TRACE_STATS_INTAKE_ROUTE: &str = "/api/v0.2/stats";

pub fn read_cloud_env() -> Option<(String, trace_utils::EnvironmentType)> {
    if let Ok(res) = env::var("AWS_LAMBDA_FUNCTION_NAME") {
        return Some((res, trace_utils::EnvironmentType::LambdaFunction));
    }
    if let Ok(res) = env::var("K_SERVICE") {
        // Set by Google Cloud Functions for newer runtimes
        return Some((res, trace_utils::EnvironmentType::CloudFunction));
    }
    if let Ok(res) = env::var("FUNCTION_NAME") {
        // Set by Google Cloud Functions for older runtimes
        return Some((res, trace_utils::EnvironmentType::CloudFunction));
    }
    if let Ok(res) = env::var("WEBSITE_SITE_NAME") {
        // Set by Azure Functions
        return Some((res, trace_utils::EnvironmentType::AzureFunction));
    }
    if let Ok(res) = env::var("ASCSVCRT_SPRING__APPLICATION__NAME") {
        // Set by Azure Spring Apps
        return Some((res, trace_utils::EnvironmentType::AzureSpringApp));
    }
    None
}

pub fn trace_intake_url(site: &str) -> String {
    construct_trace_intake_url(site, TRACE_INTAKE_ROUTE)
}

pub fn trace_intake_url_prefixed(endpoint_prefix: &str) -> String {
    format!("{endpoint_prefix}{TRACE_INTAKE_ROUTE}")
}

pub fn trace_stats_url(site: &str) -> String {
    construct_trace_intake_url(site, TRACE_STATS_INTAKE_ROUTE)
}

pub fn trace_stats_url_prefixed(endpoint_prefix: &str) -> String {
    format!("{endpoint_prefix}{TRACE_STATS_INTAKE_ROUTE}")
}

fn construct_trace_intake_url(prefix: &str, route: &str) -> String {
    format!("https://{PROD_INTAKE_SUBDOMAIN}.{prefix}{route}")
}
