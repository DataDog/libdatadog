// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use ddcommon::{parse_uri, Endpoint};
use http::{uri::PathAndQuery, Uri};
use lazy_static::lazy_static;

use std::{borrow::Cow, env, path::PathBuf, str::FromStr, time::Duration};

pub const DEFAULT_DD_SITE: &str = "datadoghq.com";
pub const PROD_INTAKE_FORMAT_PREFIX: &str = "https://instrumentation-telemetry-intake";

const DIRECT_TELEMETRY_URL_PATH: &str = "/api/v2/apmtelemetry";
const AGENT_TELEMETRY_URL_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

const DEFAULT_AGENT_HOST: &str = "localhost";
const DEFAULT_AGENT_PORT: u16 = 8126;

#[derive(Clone, Debug)]
pub struct Config {
    /// Endpoint to send the data to
    pub endpoint: Option<Endpoint>,
    /// Path to a file where the data is written instead of sent to the intake
    pub mock_client_file: Option<PathBuf>,
    /// Enables debug logging
    pub telemetry_debug_logging_enabled: bool,
    pub telemetry_hearbeat_interval: Duration,
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

// TODO Paul LGDC: if this struct carries no data, it would probably be better to have it as a module
impl FromEnv {
    const DD_APM_TELEMETRY_DD_URL: &'static str = "DD_APM_TELEMETRY_DD_URL";
    const DD_API_KEY: &'static str = "DD_API_KEY";
    const DD_AGENT_HOST: &'static str = "DD_AGENT_HOST";
    const DD_AGENT_PORT: &'static str = "DD_AGENT_PORT";
    const DD_SITE: &'static str = "DD_SITE";
    const DD_TELEMETRY_HEARTBEAT_INTERVAL: &'static str = "DD_TELEMETRY_HEARTBEAT_INTERVAL";

    // Development env variables
    const _DD_SHARED_LIB_DEBUG: &'static str = "_DD_SHARED_LIB_DEBUG";
    const _DD_DIRECT_SUBMISSION_ENABLED: &'static str = "_DD_DIRECT_SUBMISSION_ENABLED";

    fn telemetry_hearbeat_interval() -> Option<Duration> {
        Some(Duration::from_secs_f32(
            env::var(Self::DD_TELEMETRY_HEARTBEAT_INTERVAL)
                .ok()?
                .parse::<f32>()
                .ok()?,
        ))
    }

    fn agent_port() -> Option<u16> {
        env::var(Self::DD_AGENT_PORT).ok()?.parse::<u16>().ok()
    }

    fn direct_submission_enabled() -> bool {
        env::var(Self::_DD_DIRECT_SUBMISSION_ENABLED).is_ok()
    }

    fn agent_base_url() -> String {
        let agent_port = Self::agent_port().unwrap_or(DEFAULT_AGENT_PORT);
        let agent_host =
            env::var(Self::DD_AGENT_HOST).unwrap_or_else(|_| String::from(DEFAULT_AGENT_HOST));

        format!("http://{agent_host}:{agent_port}{AGENT_TELEMETRY_URL_PATH}")
    }

    fn intake_base_url() -> Option<String> {
        match env::var(Self::DD_APM_TELEMETRY_DD_URL) {
            Ok(url) if !url.is_empty() => return Some(url),
            _ => {}
        }
        match env::var(Self::DD_SITE) {
            Ok(dd_site) if !dd_site.is_empty() => {
                return Some(format!(
                    "{PROD_INTAKE_FORMAT_PREFIX}.{dd_site}{DIRECT_TELEMETRY_URL_PATH}"
                ))
            }
            _ => {}
        }
        None
    }

    fn debug_enabled() -> Option<bool> {
        let var = env::var(Self::_DD_SHARED_LIB_DEBUG).ok()?;
        Some(var == "true" || var == "1")
    }

    fn api_key() -> Option<String> {
        env::var(Self::DD_API_KEY).ok().filter(|p| !p.is_empty())
    }

    pub fn build_endpoint(agent_url: &str, api_key: Option<String>) -> Option<Endpoint> {
        let telemetry_uri = if api_key.is_some() {
            let telemetry_intake_base_url = Self::intake_base_url()?;
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

macro_rules! try_block {
    ($($st:stmt)*) => {
        (|| {$($st)*})()
    };
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: None,
            mock_client_file: None,
            telemetry_debug_logging_enabled: false,
            telemetry_hearbeat_interval: Duration::from_secs(60),
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let default = Self::default();

        let direct_submission_enabled = FromEnv::direct_submission_enabled();
        let endpoint = try_block! {if !direct_submission_enabled {
            Some(Endpoint {
                url: Uri::from_str(&FromEnv::agent_base_url()).ok()?,
                api_key: None,
            })
        } else {
            Some(Endpoint {
                url: Uri::from_str(&FromEnv::agent_base_url()).ok()?,
                api_key: FromEnv::api_key().map(Cow::Owned),
            })
        }};

        let telemetry_debug_logging_enabled =
            FromEnv::debug_enabled().unwrap_or(default.telemetry_debug_logging_enabled);
        let telemetry_hearbeat_interval =
            FromEnv::telemetry_hearbeat_interval().unwrap_or(default.telemetry_hearbeat_interval);

        Self {
            telemetry_debug_logging_enabled,
            endpoint,
            telemetry_hearbeat_interval,
            mock_client_file: None,
        }
    }

    pub fn get() -> &'static Self {
        lazy_static! {
            static ref CFG: Config = Config::from_env();
        }
        &CFG
    }

    pub fn set_url(&mut self, url: &str) -> anyhow::Result<()> {
        let uri = parse_uri(url)?;

        if let "file" = uri.scheme_str().unwrap_or_default() {
            self.endpoint = Some(Endpoint {
                url: Uri::from_static("http://datadoghq.invalid/"),
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
}

#[cfg(all(test, target_family = "unix"))]
mod test {
    use ddcommon::connector::uds;

    use super::Config;

    #[test]
    fn test_config_url_update() {
        let mut cfg = Config::default();

        cfg.set_url("http://example.com/any_path_will_be_ignored")
            .unwrap();

        assert_eq!(
            "http://example.com/telemetry/proxy/api/v2/apmtelemetry",
            cfg.clone().endpoint.unwrap().url
        );

        cfg.set_url("file:///absolute/path").unwrap();

        assert_eq!(
            "http://datadoghq.invalid/",
            cfg.clone().endpoint.unwrap().url.to_string()
        );
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
