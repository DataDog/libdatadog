// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use log::{debug, error};
use std::env;

use datadog_trace_obfuscation::replacer::{parse_raw_rules, RawReplaceRule, ReplaceRule};

const TRACE_INTAKE_ROUTE: &str = "/api/v0.2/traces";
const TRACE_STATS_INTAKE_ROUTE: &str = "/api/v0.2/stats";

#[derive(Debug, Clone)]
pub struct Config {
    pub api_key: String,
    pub gcp_function_name: Option<String>,
    pub max_request_content_length: usize,
    /// how often to flush traces, in seconds
    pub trace_flush_interval: u64,
    /// how often to flush stats, in seconds
    pub stats_flush_interval: u64,
    /// timeout for environment verification, in milliseconds
    pub verify_env_timeout: u64,
    pub trace_intake_url: String,
    pub trace_stats_intake_url: String,
    pub dd_site: String,
    pub tag_replace_rules: Option<Vec<ReplaceRule>>,
}

impl Config {
    pub fn new() -> Result<Config, Box<dyn std::error::Error>> {
        let api_key = env::var("DD_API_KEY")
            .map_err(|_| anyhow::anyhow!("DD_API_KEY environment variable is not set"))?;
        let mut function_name = None;

        // Google cloud functions automatically sets either K_SERVICE or FUNCTION_NAME
        // env vars to denote the cloud function name.
        // K_SERVICE is set on newer runtimes, while FUNCTION_NAME is set on older deprecated runtimes.
        if let Ok(res) = env::var("K_SERVICE") {
            function_name = Some(res);
        } else if let Ok(res) = env::var("FUNCTION_NAME") {
            function_name = Some(res);
        }

        let dd_site = env::var("DD_SITE").unwrap_or_else(|_| "datadoghq.com".to_string());

        let trace_intake_url = construct_trace_intake_url(&dd_site, TRACE_INTAKE_ROUTE);
        let trace_stats_intake_url = construct_trace_intake_url(&dd_site, TRACE_STATS_INTAKE_ROUTE);

        let tag_replace_rules = env::var("DD_APM_REPLACE_TAGS").map_or(None, get_tag_replace_rules);

        Ok(Config {
            api_key,
            gcp_function_name: function_name,
            max_request_content_length: 10 * 1024 * 1024, // 10MB in Bytes
            trace_flush_interval: 3,
            stats_flush_interval: 3,
            verify_env_timeout: 100,
            dd_site,
            trace_intake_url,
            trace_stats_intake_url,
            tag_replace_rules,
        })
    }
}

fn construct_trace_intake_url(prefix: &str, route: &str) -> String {
    format!("https://trace.agent.{prefix}{route}")
}

fn get_tag_replace_rules(env_var_value: String) -> Option<Vec<ReplaceRule>> {
    let replace_rules_strings: Vec<RawReplaceRule> = match serde_json::from_str(&env_var_value) {
        Ok(res) => res,
        Err(_) => {
            error!("Invalid DD_APM_REPLACE_TAGS value: Not valid Replace Tags JSON");
            return None;
        }
    };
    match parse_raw_rules(replace_rules_strings) {
        Ok(res) => {
            debug!("Successfully parsed DD_APM_REPLACE_TAGS value");
            Some(res)
        }
        Err(e) => {
            error!("Failed to parse DD_APM_REPLACE_TAGS: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use datadog_trace_obfuscation::replacer::ReplaceRule;
    use duplicate::duplicate_item;
    use log::Level;
    use regex::Regex;
    use serial_test::serial;
    use std::env;

    use crate::config::{self, get_tag_replace_rules};

    #[test]
    #[serial]
    fn test_error_if_no_api_key_env_var() {
        let config = config::Config::new();
        assert!(config.is_err());
        assert_eq!(
            config.unwrap_err().to_string(),
            "DD_API_KEY environment variable is not set"
        );
    }

    #[test]
    #[serial]
    fn test_default_trace_and_trace_stats_urls() {
        env::set_var("DD_API_KEY", "_not_a_real_key_");
        let config_res = config::Config::new();
        assert!(config_res.is_ok());
        let config = config_res.unwrap();
        assert_eq!(
            config.trace_intake_url,
            "https://trace.agent.datadoghq.com/api/v0.2/traces"
        );
        assert_eq!(
            config.trace_stats_intake_url,
            "https://trace.agent.datadoghq.com/api/v0.2/stats"
        );
        env::remove_var("DD_API_KEY");
    }

    #[duplicate_item(
        test_name                       dd_site                 expected_url;
        [test_us1_trace_intake_url]     ["datadoghq.com"]       ["https://trace.agent.datadoghq.com/api/v0.2/traces"];
        [test_us3_trace_intake_url]     ["us3.datadoghq.com"]   ["https://trace.agent.us3.datadoghq.com/api/v0.2/traces"];
        [test_us5_trace_intake_url]     ["us5.datadoghq.com"]   ["https://trace.agent.us5.datadoghq.com/api/v0.2/traces"];
        [test_eu_trace_intake_url]      ["datadoghq.eu"]        ["https://trace.agent.datadoghq.eu/api/v0.2/traces"];
        [test_ap1_trace_intake_url]     ["ap1.datadoghq.com"]   ["https://trace.agent.ap1.datadoghq.com/api/v0.2/traces"];
        [test_gov_trace_intake_url]     ["ddog-gov.com"]        ["https://trace.agent.ddog-gov.com/api/v0.2/traces"];
    )]
    #[test]
    #[serial]
    fn test_name() {
        env::set_var("DD_API_KEY", "_not_a_real_key_");
        env::set_var("DD_SITE", dd_site);
        let config_res = config::Config::new();
        assert!(config_res.is_ok());
        let config = config_res.unwrap();
        assert_eq!(config.trace_intake_url, expected_url);
        env::remove_var("DD_API_KEY");
        env::remove_var("DD_SITE");
    }

    #[duplicate_item(
        test_name                       dd_site                 expected_url;
        [test_us1_trace_stats_intake_url]     ["datadoghq.com"]       ["https://trace.agent.datadoghq.com/api/v0.2/stats"];
        [test_us3_trace_stats_intake_url]     ["us3.datadoghq.com"]   ["https://trace.agent.us3.datadoghq.com/api/v0.2/stats"];
        [test_us5_trace_stats_intake_url]     ["us5.datadoghq.com"]   ["https://trace.agent.us5.datadoghq.com/api/v0.2/stats"];
        [test_eu_trace_stats_intake_url]      ["datadoghq.eu"]        ["https://trace.agent.datadoghq.eu/api/v0.2/stats"];
        [test_ap1_trace_stats_intake_url]     ["ap1.datadoghq.com"]   ["https://trace.agent.ap1.datadoghq.com/api/v0.2/stats"];
        [test_gov_trace_stats_intake_url]     ["ddog-gov.com"]        ["https://trace.agent.ddog-gov.com/api/v0.2/stats"];
    )]
    #[test]
    #[serial]
    fn test_name() {
        env::set_var("DD_API_KEY", "_not_a_real_key_");
        env::set_var("DD_SITE", dd_site);
        let config_res = config::Config::new();
        assert!(config_res.is_ok());
        let config = config_res.unwrap();
        assert_eq!(config.trace_stats_intake_url, expected_url);
        env::remove_var("DD_API_KEY");
        env::remove_var("DD_SITE");
    }

    #[test]
    fn test_get_tag_replace_rules_invalid_json() {
        testing_logger::setup();
        let invalid_json = "{".to_string();
        let res = get_tag_replace_rules(invalid_json);
        assert!(res.is_none());
        testing_logger::validate(|captured_logs| {
            assert_eq!(captured_logs.len(), 1);
            assert_eq!(
                captured_logs[0].body,
                "Invalid DD_APM_REPLACE_TAGS value: Not valid Replace Tags JSON"
            );
            assert_eq!(captured_logs[0].level, Level::Error);
        });
    }

    #[test]
    fn test_get_tag_replace_rules_valid_json() {
        let invalid_regex =
            "[{\"name\": \"*\", \"pattern\": \"api_key\", \"repl\": \"REDACTED\"},{\"name\": \"test_name\", \"pattern\": \"asdf\", \"repl\": \"*\"}]".to_string();
        let res = get_tag_replace_rules(invalid_regex).unwrap();
        assert_eq!(res.len(), 2);
        assert_eq!(
            res,
            vec!(
                ReplaceRule {
                    name: "*".to_string(),
                    re: Regex::new("api_key").unwrap(),
                    repl: "REDACTED".to_string()
                },
                ReplaceRule {
                    name: "test_name".to_string(),
                    re: Regex::new("asdf").unwrap(),
                    repl: "*".to_string()
                }
            )
        )
    }

    #[test]
    fn test_get_tag_replace_rules_invalid_regex() {
        testing_logger::setup();
        let invalid_regex =
            "[{\"name\": \"*\", \"pattern\": \")\", \"repl\": \"REDACTED\"}]".to_string();
        let res = get_tag_replace_rules(invalid_regex);
        assert!(res.is_none());
        testing_logger::validate(|captured_logs| {
            assert_eq!(captured_logs.len(), 1);
            assert_eq!(
                captured_logs[0].body,
                "Failed to parse DD_APM_REPLACE_TAGS: regex parse error:\n    )\n    ^\nerror: unopened group"
            );
            assert_eq!(captured_logs[0].level, Level::Error);
        });
    }
}
