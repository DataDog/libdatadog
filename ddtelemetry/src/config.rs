// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use ddcommon::{parse_uri, Endpoint};
use std::{borrow::Cow, path::PathBuf, time::Duration};

use http::{uri::PathAndQuery, Uri};
use lazy_static::lazy_static;

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

fn url_with_telemetry_path(agent_url: &str) -> anyhow::Result<Uri> {
    let mut agent_uri_parts = parse_uri(agent_url)?.into_parts();
    agent_uri_parts.path_and_query = Some(PathAndQuery::from_static(AGENT_TELEMETRY_URL_PATH));

    Ok(Uri::from_parts(agent_uri_parts)?)
}

mod parse_env {
    use ddcommon::parse_uri;
    use http::Uri;
    use std::{env, str::FromStr, time::Duration};

    pub fn duration(name: &str) -> Option<Duration> {
        Some(Duration::from_secs_f32(
            env::var(name).ok()?.parse::<f32>().ok()?,
        ))
    }

    pub fn int<T: FromStr>(name: &str) -> Option<T> {
        env::var(name).ok()?.parse::<T>().ok()
    }

    pub fn bool(name: &str) -> Option<bool> {
        let var = env::var(name).ok()?;
        Some(var == "true" || var == "1")
    }

    pub fn str_not_empty(name: &str) -> Option<String> {
        env::var(name).ok().filter(|s| !s.is_empty())
    }

    pub fn uri(name: &str) -> Option<Uri> {
        parse_uri(&str_not_empty(name)?).ok()
    }
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
            mock_client_file: None,
            telemetry_debug_logging_enabled: false,
            telemetry_hearbeat_interval: Duration::from_secs(60),
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
                    "{}.{}{}",
                    PROD_INTAKE_FORMAT_PREFIX,
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

    fn set_endpoint(
        &mut self,
        url: &str,
        api_key: Option<Cow<'static, str>>,
    ) -> anyhow::Result<()> {
        if let Some(path) = url.strip_prefix("file://") {
            self.endpoint = Some(Endpoint {
                url: Uri::from_static("http://datadoghq.invalid/"),
                api_key,
            });
            self.mock_client_file = Some(path.into());
        } else {
            self.endpoint = Some(Endpoint {
                url: url_with_telemetry_path(url)?,
                api_key,
            })
        }
        Ok(())
    }

    pub fn from_settings(settings: &Settings) -> Self {
        let url = Self::url_from_settings(settings);
        let api_key = Self::api_key_from_settings(settings);

        let mut this = Self {
            endpoint: None,
            mock_client_file: None,
            telemetry_debug_logging_enabled: settings.shared_lib_debug,
            telemetry_hearbeat_interval: settings.telemetry_heartbeat_interval,
        };
        let _res = this.set_endpoint(&url, api_key);

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
        self.set_endpoint(url, api_key)
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
