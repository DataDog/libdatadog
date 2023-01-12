// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use ddcommon::{connector, Endpoint, HttpClient, HttpRequestBuilder};
use http::Uri;
use lazy_static::lazy_static;
use std::{
    borrow::{Borrow, Cow},
    env,
    str::FromStr,
    time::Duration,
};

pub const DEFAULT_DD_SITE: &str = "datadoghq.com";
pub const PROD_INTAKE_FORMAT_PREFIX: &str = "https://instrumentation-telemetry-intake";

pub const STAGING_INTAKE: &str = "https://all-http-intake.logs.datad0g.com";
const DIRECT_TELEMETRY_URL_PATH: &str = "/api/v2/apmtelemetry";
const AGENT_TELEMETRY_URL_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

const DEFAULT_AGENT_HOST: &str = "localhost";
const DEFAULT_AGENT_PORT: u16 = 8126;

const DD_APM_TELEMETRY_DD_URL: &str = "DD_APM_TELEMETRY_DD_URL";
const _DD_SHARED_LIB_DEBUG: &str = "_DD_SHARED_LIB_DEBUG";
const DD_API_KEY: &str = "DD_API_KEY";
const DD_AGENT_HOST: &str = "DD_AGENT_HOST";
const DD_AGENT_PORT: &str = "DD_AGENT_PORT";
const DD_SITE: &str = "DD_SITE";

pub struct Config {
    #[allow(dead_code)]
    agent_url: String,
    endpoint: Option<Endpoint>,
    telemetry_debug_logging_enabled: bool,
}

pub trait ProvideConfig {
    fn config() -> Config;
}

pub struct FromEnv {}

impl FromEnv {
    fn get_agent_base_url() -> String {
        let agent_port = env::var(DD_AGENT_PORT)
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(DEFAULT_AGENT_PORT);
        let agent_host =
            env::var(DD_AGENT_HOST).unwrap_or_else(|_| String::from(DEFAULT_AGENT_HOST));

        format!("http://{agent_host}:{agent_port}")
    }

    fn get_intake_base_url() -> String {
        if let Some(url) = env::var(DD_APM_TELEMETRY_DD_URL)
            .ok()
            .filter(|s| !s.is_empty())
        {
            return url;
        }

        if let Ok(dd_site) = env::var(DD_SITE) {
            if dd_site.is_empty() {
                format!("{PROD_INTAKE_FORMAT_PREFIX}.{DEFAULT_DD_SITE}")
            } else {
                format!("{PROD_INTAKE_FORMAT_PREFIX}.{dd_site}")
            }
        } else {
            String::from(STAGING_INTAKE)
        }
    }

    fn build_endpoint(agent_url: &str) -> Option<Endpoint> {
        let api_key = env::var(DD_API_KEY).ok().filter(|p| !p.is_empty());

        let telemetry_url = if api_key.is_some() {
            let telemetry_intake_base_url = Self::get_intake_base_url();
            format!("{telemetry_intake_base_url}{DIRECT_TELEMETRY_URL_PATH}")
        } else {
            format!("{}{AGENT_TELEMETRY_URL_PATH}", &agent_url)
        };

        let telemetry_uri = Uri::from_str(&telemetry_url).ok()?;
        Some(Endpoint {
            url: telemetry_uri,
            api_key: api_key.map(|v| v.into()),
        })
    }
}

impl ProvideConfig for FromEnv {
    fn config() -> Config {
        let agent_url = Self::get_agent_base_url();
        let endpoint = Self::build_endpoint(&agent_url);
        let debug_enabled = env::var(_DD_SHARED_LIB_DEBUG)
            .ok()
            .and_then(|x| {
                match x.parse::<bool>() {
                    Ok(v) => Ok(v),
                    Err(_) => x.parse::<u32>().map(|x| x == 1u32),
                }
                .ok()
            })
            .unwrap_or(false);

        Config {
            agent_url,
            telemetry_debug_logging_enabled: debug_enabled,
            endpoint,
        }
    }
}

impl Config {
    pub fn get() -> &'static Self {
        lazy_static! {
            static ref CFG: Config = FromEnv::config();
        }
        &CFG
    }

    pub fn is_telemetry_debug_logging_enabled(&self) -> bool {
        self.telemetry_debug_logging_enabled
    }

    pub fn api_key(&self) -> Option<Cow<str>> {
        self.endpoint.as_ref()?.api_key.clone() //TODO remove this getter
    }

    pub fn endpoint(&self) -> &Option<Endpoint> {
        self.endpoint.borrow()
    }

    pub fn http_client(&self) -> HttpClient {
        hyper::Client::builder()
            .pool_idle_timeout(Duration::from_secs(30))
            .build(connector::Connector::new())
    }

    pub fn into_request_builder(&self) -> anyhow::Result<HttpRequestBuilder> {
        match self.endpoint() {
            Some(e) => e.into_request_builder(concat!("telemetry/", env!("CARGO_PKG_VERSION"))),
            None => Err(anyhow::Error::msg(
                "no valid endpoint found, can't build the request".to_string(),
            )),
        }
    }

    pub fn is_direct(&self) -> bool {
        self.api_key().is_some() // If API key is provided call directly
    }
}
