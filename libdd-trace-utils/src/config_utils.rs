// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::trace_utils;
use std::env;
use tracing::{debug, error};

pub const PROD_INTAKE_SUBDOMAIN: &str = "trace.agent";

const TRACE_INTAKE_ROUTE: &str = "/api/v0.2/traces";
const TRACE_STATS_INTAKE_ROUTE: &str = "/api/v0.2/stats";

pub fn read_cloud_env() -> Option<(String, trace_utils::EnvironmentType)> {
    let mut detected: Vec<(String, trace_utils::EnvironmentType)> = Vec::new();

    if env::var("AWS_LAMBDA_INITIALIZATION_TYPE").is_ok() {
        match env::var("AWS_LAMBDA_FUNCTION_NAME") {
            Ok(name) => detected.push((name, trace_utils::EnvironmentType::LambdaFunction)),
            Err(_) => {
                error!("AWS Lambda environment detected but AWS_LAMBDA_FUNCTION_NAME is not set");
            }
        }
    }

    if env::var("FUNCTIONS_EXTENSION_VERSION").is_ok()
        && env::var("FUNCTIONS_WORKER_RUNTIME").is_ok()
    {
        match env::var("WEBSITE_SITE_NAME") {
            Ok(name) => detected.push((name, trace_utils::EnvironmentType::AzureFunction)),
            Err(_) => {
                error!("Azure Functions environment detected but WEBSITE_SITE_NAME is not set");
            }
        }
    }

    if let (Ok(name), Ok(_)) = (env::var("K_SERVICE"), env::var("FUNCTION_TARGET")) {
        // Set by Google Cloud Functions for newer runtimes
        detected.push((name, trace_utils::EnvironmentType::CloudFunction));
    } else if let (Ok(name), Ok(_)) = (env::var("FUNCTION_NAME"), env::var("GCP_PROJECT")) {
        // Set by Google Cloud Functions for older runtimes
        detected.push((name, trace_utils::EnvironmentType::CloudFunction));
    }

    if let Ok(name) = env::var("ASCSVCRT_SPRING__APPLICATION__NAME") {
        // Set by Azure Spring Apps
        detected.push((name, trace_utils::EnvironmentType::AzureSpringApp));
    }

    match detected.len() {
        0 => {
            error!("No cloud environment detected");
            None
        }
        1 => {
            let (ref name, ref env_type) = detected[0];
            debug!("Cloud environment detected: {env_type:?} ({name})");
            detected.into_iter().next()
        }
        _ => {
            let env_names: Vec<String> = detected
                .iter()
                .map(|(name, env_type)| format!("{env_type:?}({name})"))
                .collect();
            error!(
                "Multiple cloud environments detected: {}",
                env_names.join(", ")
            );
            None
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_VARS: &[&str] = &[
        "AWS_LAMBDA_INITIALIZATION_TYPE",
        "AWS_LAMBDA_FUNCTION_NAME",
        "FUNCTIONS_EXTENSION_VERSION",
        "FUNCTIONS_WORKER_RUNTIME",
        "WEBSITE_SITE_NAME",
        "FUNCTION_NAME",
        "GCP_PROJECT",
        "K_SERVICE",
        "FUNCTION_TARGET",
        "ASCSVCRT_SPRING__APPLICATION__NAME",
    ];

    /// RAII guard that restores an environment variable to its previous value when dropped.
    struct EnvGuard {
        key: &'static str,
        saved: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let saved = env::var(key).ok();
            unsafe { env::set_var(key, value) };
            EnvGuard { key, saved }
        }

        fn remove(key: &'static str) -> Self {
            let saved = env::var(key).ok();
            unsafe { env::remove_var(key) };
            EnvGuard { key, saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.saved {
                Some(s) => unsafe { env::set_var(self.key, s) },
                None => unsafe { env::remove_var(self.key) },
            }
        }
    }

    fn clear_all_env_vars() -> Vec<EnvGuard> {
        ALL_VARS.iter().map(|k| EnvGuard::remove(k)).collect()
    }

    #[test]
    #[serial_test::serial]
    fn test_aws_lambda_detected() {
        let _guards = clear_all_env_vars();
        let _init = EnvGuard::set("AWS_LAMBDA_INITIALIZATION_TYPE", "on-demand");
        let _name = EnvGuard::set("AWS_LAMBDA_FUNCTION_NAME", "my-function");
        let result = read_cloud_env();
        assert_eq!(
            result,
            Some((
                "my-function".to_string(),
                trace_utils::EnvironmentType::LambdaFunction
            ))
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_aws_lambda_missing_function_name() {
        let _guards = clear_all_env_vars();
        let _init = EnvGuard::set("AWS_LAMBDA_INITIALIZATION_TYPE", "on-demand");
        let result = read_cloud_env();
        assert_eq!(result, None);
    }

    #[test]
    #[serial_test::serial]
    fn test_aws_lambda_not_detected_without_init_type() {
        let _guards = clear_all_env_vars();
        let _name = EnvGuard::set("AWS_LAMBDA_FUNCTION_NAME", "my-function");
        let result = read_cloud_env();
        assert_eq!(result, None);
    }

    #[test]
    #[serial_test::serial]
    fn test_azure_function_detected() {
        let _guards = clear_all_env_vars();
        let _ext = EnvGuard::set("FUNCTIONS_EXTENSION_VERSION", "~4");
        let _rt = EnvGuard::set("FUNCTIONS_WORKER_RUNTIME", "java");
        let _site = EnvGuard::set("WEBSITE_SITE_NAME", "my-azure-app");
        let result = read_cloud_env();
        assert_eq!(
            result,
            Some((
                "my-azure-app".to_string(),
                trace_utils::EnvironmentType::AzureFunction
            ))
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_azure_function_missing_site_name() {
        let _guards = clear_all_env_vars();
        let _ext = EnvGuard::set("FUNCTIONS_EXTENSION_VERSION", "~4");
        let _rt = EnvGuard::set("FUNCTIONS_WORKER_RUNTIME", "java");
        let result = read_cloud_env();
        assert_eq!(result, None);
    }

    #[test]
    #[serial_test::serial]
    fn test_azure_function_not_detected_with_only_one_var() {
        let _guards = clear_all_env_vars();
        let _ext = EnvGuard::set("FUNCTIONS_EXTENSION_VERSION", "~4");
        let result = read_cloud_env();
        assert_eq!(result, None);
    }

    #[test]
    #[serial_test::serial]
    fn test_gcp_1st_gen_detected() {
        let _guards = clear_all_env_vars();
        let _name = EnvGuard::set("FUNCTION_NAME", "my-gcp-function");
        let _project = EnvGuard::set("GCP_PROJECT", "my-project");
        let result = read_cloud_env();
        assert_eq!(
            result,
            Some((
                "my-gcp-function".to_string(),
                trace_utils::EnvironmentType::CloudFunction
            ))
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_gcp_1st_gen_not_detected_without_gcp_project() {
        let _guards = clear_all_env_vars();
        let _name = EnvGuard::set("FUNCTION_NAME", "my-gcp-function");
        let result = read_cloud_env();
        assert_eq!(result, None);
    }

    #[test]
    #[serial_test::serial]
    fn test_gcp_2nd_gen_detected() {
        let _guards = clear_all_env_vars();
        let _service = EnvGuard::set("K_SERVICE", "my-cloud-run-fn");
        let _target = EnvGuard::set("FUNCTION_TARGET", "myHandler");
        let result = read_cloud_env();
        assert_eq!(
            result,
            Some((
                "my-cloud-run-fn".to_string(),
                trace_utils::EnvironmentType::CloudFunction
            ))
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_gcp_2nd_gen_not_detected_without_function_target() {
        let _guards = clear_all_env_vars();
        let _service = EnvGuard::set("K_SERVICE", "my-cloud-run-fn");
        let result = read_cloud_env();
        assert_eq!(result, None);
    }

    #[test]
    #[serial_test::serial]
    fn test_azure_spring_app_detected() {
        let _guards = clear_all_env_vars();
        let _app = EnvGuard::set("ASCSVCRT_SPRING__APPLICATION__NAME", "my-spring-app");
        let result = read_cloud_env();
        assert_eq!(
            result,
            Some((
                "my-spring-app".to_string(),
                trace_utils::EnvironmentType::AzureSpringApp
            ))
        );
    }

    #[test]
    #[serial_test::serial]
    fn test_no_environment_detected() {
        let _guards = clear_all_env_vars();
        let result = read_cloud_env();
        assert_eq!(result, None);
    }

    #[test]
    #[serial_test::serial]
    fn test_multiple_environments_returns_none() {
        let _guards = clear_all_env_vars();
        let _init = EnvGuard::set("AWS_LAMBDA_INITIALIZATION_TYPE", "on-demand");
        let _fn_name = EnvGuard::set("AWS_LAMBDA_FUNCTION_NAME", "my-lambda");
        let _ext = EnvGuard::set("FUNCTIONS_EXTENSION_VERSION", "~4");
        let _rt = EnvGuard::set("FUNCTIONS_WORKER_RUNTIME", "java");
        let _site = EnvGuard::set("WEBSITE_SITE_NAME", "my-azure-app");
        let result = read_cloud_env();
        assert_eq!(result, None);
    }
}
