// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon::{config::parse_env, parse_uri, Endpoint};
use std::{borrow::Cow, time::Duration};

use http::{uri::PathAndQuery, Uri};
use lazy_static::lazy_static;

pub const DEFAULT_DD_SITE: &str = "datadoghq.com";
pub const PROD_INTAKE_SUBDOMAIN: &str = "instrumentation-telemetry-intake";

const DIRECT_TELEMETRY_URL_PATH: &str = "/api/v2/apmtelemetry";
const AGENT_TELEMETRY_URL_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

const DEFAULT_AGENT_HOST: &str = "localhost";
const DEFAULT_AGENT_PORT: u16 = 8126;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Config {
    /// Endpoint to send the data to
    pub endpoint: Option<Endpoint>,
    /// Enables debug logging
    pub telemetry_debug_logging_enabled: bool,
    pub telemetry_hearbeat_interval: Duration,
    pub direct_submission_enabled: bool,
    /// Prevents LifecycleAction::Stop from terminating the worker (except if the WorkerHandle is
    /// dropped)
    pub restartable: bool,
}

fn endpoint_with_telemetry_path(
    mut endpoint: Endpoint,
    direct_submission_enabled: bool,
) -> anyhow::Result<Endpoint> {
    let mut uri_parts = endpoint.url.into_parts();
    if uri_parts.scheme.is_some() && uri_parts.scheme.as_ref().unwrap().as_str() != "file" {
        uri_parts.path_and_query = Some(PathAndQuery::from_static(
            if endpoint.api_key.is_some() && direct_submission_enabled {
                DIRECT_TELEMETRY_URL_PATH
            } else {
                AGENT_TELEMETRY_URL_PATH
            },
        ));
    }

    endpoint.url = Uri::from_parts(uri_parts)?;
    Ok(endpoint)
}

/// Settings gathers configuration options we receive from the environment
/// (either through env variable, or that could be set from the )
pub struct Settings {
    pub agent_host: String,
    pub trace_agent_port: u16,
    pub trace_agent_url: Option<Uri>,
    pub direct_submission_enabled: bool,
    pub api_key: Option<String>,
    pub site: Option<String>,
    pub telemetry_dd_url: Option<String>,
    pub telemetry_heartbeat_interval: Duration,
    pub telemetry_extended_heartbeat_interval: Duration,
    pub shared_lib_debug: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            agent_host: DEFAULT_AGENT_HOST.to_owned(),
            trace_agent_port: DEFAULT_AGENT_PORT.to_owned(),
            trace_agent_url: None,
            direct_submission_enabled: false,
            api_key: None,
            site: None,
            telemetry_dd_url: None,
            telemetry_heartbeat_interval: Duration::from_secs(60),
            telemetry_extended_heartbeat_interval: Duration::from_secs(60 * 60 * 24),
            shared_lib_debug: false,
        }
    }
}

impl Settings {
    // Agent connection configuration
    const DD_AGENT_HOST: &'static str = "DD_AGENT_HOST";
    const DD_TRACE_AGENT_PORT: &'static str = "DD_TRACE_AGENT_PORT";
    const DD_TRACE_AGENT_URL: &'static str = "DD_TRACE_AGENT_URL";

    // Direct submission configuration
    const _DD_DIRECT_SUBMISSION_ENABLED: &'static str = "_DD_DIRECT_SUBMISSION_ENABLED";
    const DD_API_KEY: &'static str = "DD_API_KEY";
    const DD_SITE: &'static str = "DD_SITE";
    const DD_APM_TELEMETRY_DD_URL: &'static str = "DD_APM_TELEMETRY_DD_URL";

    // Development and test env variables - should not be used by customers
    const DD_TELEMETRY_HEARTBEAT_INTERVAL: &'static str = "DD_TELEMETRY_HEARTBEAT_INTERVAL";
    const DD_TELEMETRY_EXTENDED_HEARTBEAT_INTERVAL: &'static str =
        "DD_TELEMETRY_EXTENDED_HEARTBEAT_INTERVAL";
    const _DD_SHARED_LIB_DEBUG: &'static str = "_DD_SHARED_LIB_DEBUG";

    pub fn from_env() -> Self {
        let default = Self::default();
        Self {
            agent_host: parse_env::str_not_empty(Self::DD_AGENT_HOST).unwrap_or(default.agent_host),
            trace_agent_port: parse_env::int(Self::DD_TRACE_AGENT_PORT)
                .unwrap_or(default.trace_agent_port),
            trace_agent_url: parse_env::uri(Self::DD_TRACE_AGENT_URL).or(default.trace_agent_url),
            direct_submission_enabled: parse_env::bool(Self::_DD_DIRECT_SUBMISSION_ENABLED)
                .unwrap_or(default.direct_submission_enabled),
            api_key: parse_env::str_not_empty(Self::DD_API_KEY),
            site: parse_env::str_not_empty(Self::DD_SITE),
            telemetry_dd_url: parse_env::str_not_empty(Self::DD_APM_TELEMETRY_DD_URL),
            telemetry_heartbeat_interval: parse_env::duration(
                Self::DD_TELEMETRY_HEARTBEAT_INTERVAL,
            )
            .unwrap_or(Duration::from_secs(60)),
            telemetry_extended_heartbeat_interval: parse_env::duration(
                Self::DD_TELEMETRY_EXTENDED_HEARTBEAT_INTERVAL,
            )
            .unwrap_or(Duration::from_secs(60 * 60 * 24)),
            shared_lib_debug: parse_env::bool(Self::_DD_SHARED_LIB_DEBUG).unwrap_or(false),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint: None,
            telemetry_debug_logging_enabled: false,
            telemetry_hearbeat_interval: Duration::from_secs(60),
            direct_submission_enabled: false,
            restartable: false,
        }
    }
}

impl Config {
    fn url_from_settings(settings: &Settings) -> String {
        None.or_else(|| {
            if !settings.direct_submission_enabled || settings.api_key.is_none() {
                return None;
            }
            settings.telemetry_dd_url.clone().or_else(|| {
                Some(format!(
                    "https://{}.{}{}",
                    PROD_INTAKE_SUBDOMAIN,
                    settings.site.as_ref()?,
                    DIRECT_TELEMETRY_URL_PATH
                ))
            })
        })
        .unwrap_or_else(|| {
            format!(
                "http://{}:{}{}",
                settings.agent_host, settings.trace_agent_port, AGENT_TELEMETRY_URL_PATH
            )
        })
    }

    fn api_key_from_settings(settings: &Settings) -> Option<Cow<'static, str>> {
        if !settings.direct_submission_enabled {
            return None;
        }
        settings.api_key.clone().map(Cow::Owned)
    }

    pub fn set_endpoint(&mut self, endpoint: Endpoint) -> anyhow::Result<()> {
        self.endpoint = Some(endpoint_with_telemetry_path(
            endpoint,
            self.direct_submission_enabled,
        )?);
        Ok(())
    }

    pub fn from_settings(settings: &Settings) -> Self {
        let url = Self::url_from_settings(settings);
        let api_key = Self::api_key_from_settings(settings);

        let mut this = Self {
            endpoint: None,
            telemetry_debug_logging_enabled: settings.shared_lib_debug,
            telemetry_hearbeat_interval: settings.telemetry_heartbeat_interval,
            direct_submission_enabled: settings.direct_submission_enabled,
            restartable: false,
        };
        if let Ok(url) = parse_uri(&url) {
            let _res = this.set_endpoint(Endpoint { url, api_key });
        }

        this
    }

    pub fn from_env() -> Self {
        let settings = Settings::from_env();
        Self::from_settings(&settings)
    }

    pub fn get() -> &'static Self {
        lazy_static! {
            static ref CFG: Config = Config::from_env();
        }
        &CFG
    }

    pub fn set_url(&mut self, url: &str) -> anyhow::Result<()> {
        let api_key = self.endpoint.take().and_then(|e| e.api_key);
        self.set_endpoint(Endpoint {
            url: parse_uri(url)?,
            api_key,
        })
    }
}

#[cfg(all(test, target_family = "unix"))]
mod tests {
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
            "file",
            cfg.clone()
                .endpoint
                .unwrap()
                .url
                .scheme()
                .unwrap()
                .to_string()
        );
        assert_eq!(
            "/absolute/path",
            cfg.clone()
                .endpoint
                .unwrap()
                .url
                .into_parts()
                .path_and_query
                .unwrap()
                .as_str()
        );

        cfg.set_url("file://./relative/path").unwrap();
        assert_eq!(
            "./relative/path",
            cfg.clone()
                .endpoint
                .unwrap()
                .url
                .into_parts()
                .path_and_query
                .unwrap()
                .as_str()
        );

        cfg.set_url("file://relative/path").unwrap();
        assert_eq!(
            "relative/path",
            cfg.clone()
                .endpoint
                .unwrap()
                .url
                .into_parts()
                .path_and_query
                .unwrap()
                .as_str()
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
