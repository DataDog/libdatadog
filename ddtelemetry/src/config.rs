// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use ddcommon::{connector, parse_uri, Endpoint, HttpClient, HttpRequestBuilder};
use http::{uri::PathAndQuery, Uri};
use lazy_static::lazy_static;

use std::{
    borrow::{Borrow, Cow},
    env,
    path::PathBuf,
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

#[derive(Clone, Debug)]
pub struct Config {
    pub endpoint: Option<Endpoint>,
    pub mock_client_file: Option<PathBuf>,
    pub telemetry_debug_logging_enabled: bool,
}

pub trait ProvideConfig {
    fn config() -> Config;
}

fn build_full_telemetry_agent_url(agent_url: &str) -> anyhow::Result<Uri> {
    let mut agent_uri_parts = parse_uri(agent_url)?.into_parts();
    agent_uri_parts.path_and_query = Some(PathAndQuery::from_static(AGENT_TELEMETRY_URL_PATH));

    Ok(Uri::from_parts(agent_uri_parts)?)
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

    fn get_api_key() -> Option<String> {
        env::var(DD_API_KEY).ok().filter(|p| !p.is_empty())
    }

    pub fn build_endpoint(agent_url: &str, api_key: Option<String>) -> Option<Endpoint> {
        let telemetry_uri = if api_key.is_some() {
            let telemetry_intake_base_url = Self::get_intake_base_url();
            Uri::from_str(
                format!("{telemetry_intake_base_url}{DIRECT_TELEMETRY_URL_PATH}").as_str(),
            )
            .ok()?
        } else {
            build_full_telemetry_agent_url(agent_url).ok()?
        };

        Some(Endpoint {
            url: telemetry_uri,
            api_key: api_key.map(|v| v.into()),
        })
    }
}

impl ProvideConfig for FromEnv {
    fn config() -> Config {
        let agent_url = Self::get_agent_base_url();
        let api_key = Self::get_api_key();
        let endpoint = Self::build_endpoint(&agent_url, api_key);
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
            telemetry_debug_logging_enabled: debug_enabled,
            mock_client_file: None,
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

    pub fn set_url(&mut self, url: &str) -> anyhow::Result<()> {
        let uri = parse_uri(url)?;

        if let "file" = uri.scheme_str().unwrap_or_default() {
            self.endpoint = Some(Endpoint {
                url: Uri::from_static("http://mock_endpoint/"),
                api_key: None,
            });
            self.mock_client_file = Some(uri.path().into());
        } else {
            self.endpoint = Some(Endpoint {
                url: build_full_telemetry_agent_url(url)?,
                api_key: None,
            })
        }
        Ok(())
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

#[cfg(test)]
mod test {
    use ddcommon::{connector::uds};

    use super::Config;

    #[test]
    fn test_config_url_update() {
        let mut cfg = Config {
            endpoint: None,
            mock_client_file: None,
            telemetry_debug_logging_enabled: false,
        };

        cfg.set_url("http://example.com/any_path_will_be_ignored")
            .unwrap();

        assert_eq!(
            "http://example.com/telemetry/proxy/api/v2/apmtelemetry",
            cfg.clone().endpoint.unwrap().url
        );

        cfg.set_url("file:///absolute/path").unwrap();

        assert_eq!("http://mock_endpoint/", cfg.clone().endpoint.unwrap().url.to_string());
        assert_eq!(
            "/absolute/path",
            cfg.clone().mock_client_file.unwrap().to_string_lossy()
        );

        cfg.set_url("file://./relative/path").unwrap();
        assert_eq!(
            "./relative/path",
            cfg.clone().mock_client_file.unwrap().to_string_lossy()
        );

        cfg.set_url("file://relative/path").unwrap();
        assert_eq!(
            "relative/path",
            cfg.clone().mock_client_file.unwrap().to_string_lossy()
        );

        cfg.set_url("unix:///compatiliby/path").unwrap();
        assert_eq!(
            "unix://2f636f6d706174696c6962792f70617468/telemetry/proxy/api/v2/apmtelemetry",
            cfg.clone().endpoint.unwrap().url.to_string()
        );
        assert_eq!(
            "/compatiliby/path",
            uds::socket_path_from_uri(&cfg.clone().endpoint.unwrap().url)
                .unwrap()
                .to_string_lossy()
        );
    }
}
