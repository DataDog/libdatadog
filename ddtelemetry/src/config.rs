// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use lazy_static::lazy_static;
use std::env;

pub const DEFAULT_DD_SITE: &str = "datadoghq.com";
pub const PROD_INTAKE_FORMAT_PREFIX: &str = "https://instrumentation-telemetry-intake";

pub const STAGING_INTAKE: &str = "https://all-http-intake.logs.datad0g.com";
const DIRECT_TELEMETRY_URL_PATH: &str = "/api/v2/apmtelemetry";
const AGENT_TELEMETRY_URL_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

const DEFAULT_AGENT_HOST: &str = "localhost";
const DEFAULT_AGENT_PORT: u16 = 8126;

pub struct Config {
    api_key: Option<String>,
    #[allow(dead_code)]
    agent_url: String,
    telemetry_url: String,
    telemetry_debug_logging_enabled: bool,
}

fn get_agent_base_url() -> String {
    let agent_port = env::var("DD_AGENT_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(DEFAULT_AGENT_PORT);
    let agent_host = env::var("DD_AGENT_HOST").unwrap_or_else(|_| String::from(DEFAULT_AGENT_HOST));

    format!("http://{}:{}", agent_host, agent_port)
}

fn get_intake_base_url() -> String {
    //TODO: support dd_site and additional endpoitns configuration
    if let Some(url) = env::var("DD_APM_TELEMETRY_DD_URL")
        .ok()
        .filter(|s| !s.is_empty())
    {
        return url;
    }

    if let Ok(dd_site) = env::var("DD_SITE") {
        if dd_site.is_empty() {
            format!("{}.{}", PROD_INTAKE_FORMAT_PREFIX, DEFAULT_DD_SITE)
        } else {
            format!("{}.{}", PROD_INTAKE_FORMAT_PREFIX, dd_site)
        }
    } else {
        String::from(STAGING_INTAKE)
    }
}

impl Config {
    pub fn get() -> &'static Self {
        lazy_static! {
            static ref CFG: Config = Config::read_env_config();
        }
        &CFG
    }
    pub fn read_env_config() -> Self {
        let api_key = env::var("DD_API_KEY").ok().filter(|p| !p.is_empty());
        let agent_url = get_agent_base_url();
        let telemetry_url = if api_key.is_some() {
            let telemetry_intake_base_url = get_intake_base_url();
            format!("{}{}", telemetry_intake_base_url, DIRECT_TELEMETRY_URL_PATH)
        } else {
            format!("{}{}", &agent_url, AGENT_TELEMETRY_URL_PATH)
        };
        Config {
            api_key,
            agent_url,
            telemetry_url,
            telemetry_debug_logging_enabled: false,
        }
    }

    pub fn is_telemetry_debug_logging_enabled(&self) -> bool {
        self.telemetry_debug_logging_enabled
    }

    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    pub fn telemetry_url(&self) -> &str {
        &self.telemetry_url
    }

    pub fn is_direct(&self) -> bool {
        self.api_key.is_some() // If API key is provided call directly
    }
}
