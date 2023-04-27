// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub api_key: String,
    pub gcp_function_name: Option<String>,
    pub max_request_content_length: usize,
    /// how often to flush traces, in seconds
    pub trace_flush_interval: u64,
    /// how often to flush stats, in seconds
    pub stats_flush_interval: u64,
}

impl Config {
    pub fn new() -> Result<Config, Box<dyn std::error::Error>> {
        let api_key = env::var("DD_API_KEY")?;
        let mut function_name = None;

        // Google cloud functions automatically sets either K_SERVICE or FUNCTION_NAME
        // env vars to denote the cloud function name.
        // K_SERVICE is set on newer runtimes, while FUNCTION_NAME is set on older deprecated runtimes.
        if let Ok(res) = env::var("K_SERVICE") {
            function_name = Some(res);
        } else if let Ok(res) = env::var("FUNCTION_NAME") {
            function_name = Some(res);
        }
        Ok(Config {
            api_key,
            gcp_function_name: function_name,
            max_request_content_length: 10 * 1024 * 1024, // 10MB in Bytes
            trace_flush_interval: 3,
            stats_flush_interval: 3,
        })
    }
}
