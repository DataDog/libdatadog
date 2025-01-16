// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use ddcommon::config::parse_env;
use std::{borrow::Cow, time::Duration};

use ddcommon_net1::{parse_uri, Endpoint};
use http::{uri::PathAndQuery, Uri};
use lazy_static::lazy_static;

pub const DEFAULT_DD_SITE: &str = "datadoghq.com";
pub const PROD_INTAKE_SUBDOMAIN: &str = "instrumentation-telemetry-intake";

const DIRECT_TELEMETRY_URL_PATH: &str = "/api/v2/apmtelemetry";
const AGENT_TELEMETRY_URL_PATH: &str = "/telemetry/proxy/api/v2/apmtelemetry";

#[cfg(unix)]
const TRACE_SOCKET_PATH: &str = "/var/run/datadog/apm.socket";

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
#[derive(Debug)]
pub struct Settings {
    // Env parameter
    pub agent_host: Option<String>,
    pub trace_agent_port: Option<u16>,
    pub trace_agent_url: Option<String>,
    pub trace_pipe_name: Option<String>,
    pub direct_submission_enabled: bool,
    pub api_key: Option<String>,
    pub site: Option<String>,
    pub telemetry_dd_url: Option<String>,
    pub telemetry_heartbeat_interval: Duration,
    pub telemetry_extended_heartbeat_interval: Duration,
    pub shared_lib_debug: bool,

    // Filesystem check
    pub agent_uds_socket_found: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            agent_host: None,
            trace_agent_port: None,
            trace_agent_url: None,
            trace_pipe_name: None,
            direct_submission_enabled: false,
            api_key: None,
            site: None,
            telemetry_dd_url: None,
            telemetry_heartbeat_interval: Duration::from_secs(60),
            telemetry_extended_heartbeat_interval: Duration::from_secs(60 * 60 * 24),
            shared_lib_debug: false,

            agent_uds_socket_found: false,
        }
    }
}

impl Settings {
    // Agent connection configuration
    const DD_TRACE_AGENT_URL: &'static str = "DD_TRACE_AGENT_URL";
    const DD_AGENT_HOST: &'static str = "DD_AGENT_HOST";
    const DD_TRACE_AGENT_PORT: &'static str = "DD_TRACE_AGENT_PORT";
    // Location of the named pipe on windows. Dotnet specific
    const DD_TRACE_PIPE_NAME: &'static str = "DD_TRACE_PIPE_NAME";

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
            agent_host: parse_env::str_not_empty(Self::DD_AGENT_HOST),
            trace_agent_port: parse_env::int(Self::DD_TRACE_AGENT_PORT),
            trace_agent_url: parse_env::str_not_empty(Self::DD_TRACE_AGENT_URL)
                .or(default.trace_agent_url),
            trace_pipe_name: parse_env::str_not_empty(Self::DD_TRACE_PIPE_NAME)
                .or(default.trace_pipe_name),
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

            agent_uds_socket_found: (|| {
                #[cfg(unix)]
                return std::fs::metadata(TRACE_SOCKET_PATH).is_ok();
                #[cfg(not(unix))]
                return false;
            })(),
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
    // Implemented following
    // https://github.com/DataDog/architecture/blob/master/rfcs/apm/integrations/trace-autodetect-agent-config/rfc.md
    fn trace_agent_url_from_setting(settings: &Settings) -> String {
        None.or_else(|| {
            settings
                .trace_agent_url
                .as_deref()
                .filter(|u| {
                    u.starts_with("unix://")
                        || u.starts_with("http://")
                        || u.starts_with("https://")
                })
                .map(ToString::to_string)
        })
        .or_else(|| {
            #[cfg(windows)]
            return settings
                .trace_pipe_name
                .as_ref()
                .map(|pipe_name| format!("windows:{pipe_name}"));
            #[cfg(not(windows))]
            return None;
        })
        .or_else(|| match (&settings.agent_host, settings.trace_agent_port) {
            (None, None) => None,
            _ => Some(format!(
                "http://{}:{}",
                settings.agent_host.as_deref().unwrap_or(DEFAULT_AGENT_HOST),
                settings.trace_agent_port.unwrap_or(DEFAULT_AGENT_PORT),
            )),
        })
        .or_else(|| {
            #[cfg(unix)]
            return settings
                .agent_uds_socket_found
                .then(|| format!("unix://{TRACE_SOCKET_PATH}"));
            #[cfg(not(unix))]
            return None;
        })
        .unwrap_or_else(|| format!("http://{DEFAULT_AGENT_HOST}:{DEFAULT_AGENT_PORT}"))
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
        let trace_agent_url = Self::trace_agent_url_from_setting(settings);
        let api_key = Self::api_key_from_settings(settings);

        let mut this = Self {
            endpoint: None,
            telemetry_debug_logging_enabled: settings.shared_lib_debug,
            telemetry_hearbeat_interval: settings.telemetry_heartbeat_interval,
            direct_submission_enabled: settings.direct_submission_enabled,
            restartable: false,
        };
        if let Ok(url) = parse_uri(&trace_agent_url) {
            let _res = this.set_endpoint(Endpoint {
                url,
                api_key,
                ..Default::default()
            });
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

    /// set_host sets the host telemetry should connect to.
    ///
    /// It handles the following schemes
    /// * http/https
    /// * unix sockets unix://\<path to the socket>
    /// * windows pipes of the format windows:\<pipe name>
    /// * files, with the format file://\<path to the file>
    ///
    ///  If the host_url is http/https, any path will be ignored and replaced by the
    /// appropriate telemetry endpoint path
    pub fn set_host_from_url(&mut self, host_url: &str) -> anyhow::Result<()> {
        let endpoint = self.endpoint.take().unwrap_or_default();

        self.set_endpoint(Endpoint {
            url: parse_uri(host_url)?,
            ..endpoint
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, Settings};
    use ddcommon_net1::connector::named_pipe;
    use std::path::Path;

    #[cfg(unix)]
    use ddcommon_net1::connector::uds;

    #[test]
    fn test_agent_host_detection_trace_agent_url_should_take_precedence() {
        let cases = [
            (
                "http://localhost:1234",
                "http://localhost:1234/telemetry/proxy/api/v2/apmtelemetry",
            ),
            (
                "unix://./here",
                "unix://2e2f68657265/telemetry/proxy/api/v2/apmtelemetry",
            ),
        ];
        for (trace_agent_url, expected) in cases {
            let settings = Settings {
                trace_agent_url: Some(trace_agent_url.to_owned()),
                agent_host: Some("example.org".to_owned()),
                trace_agent_port: Some(1),
                trace_pipe_name: Some("C:\\foo".to_owned()),
                agent_uds_socket_found: true,
                ..Default::default()
            };
            let cfg = Config::from_settings(&settings);
            assert_eq!(cfg.endpoint.unwrap().url.to_string(), expected);
        }
    }

    #[test]
    fn test_agent_host_detection_agent_host_and_port() {
        let cases = [
            (
                Some("example.org"),
                Some(1),
                "http://example.org:1/telemetry/proxy/api/v2/apmtelemetry",
            ),
            (
                Some("example.org"),
                None,
                "http://example.org:8126/telemetry/proxy/api/v2/apmtelemetry",
            ),
            (
                None,
                Some(1),
                "http://localhost:1/telemetry/proxy/api/v2/apmtelemetry",
            ),
        ];
        for (agent_host, trace_agent_port, expected) in cases {
            let settings = Settings {
                trace_agent_url: None,
                agent_host: agent_host.map(ToString::to_string),
                trace_agent_port,
                trace_pipe_name: None,
                agent_uds_socket_found: true,
                ..Default::default()
            };
            let cfg = Config::from_settings(&settings);
            assert_eq!(cfg.endpoint.unwrap().url.to_string(), expected);
        }
    }

    #[test]
    #[cfg(unix)]
    fn test_agent_host_detection_socket_found() {
        let settings = Settings {
            trace_agent_url: None,
            agent_host: None,
            trace_agent_port: None,
            trace_pipe_name: None,
            agent_uds_socket_found: true,
            ..Default::default()
        };
        let cfg = Config::from_settings(&settings);
        assert_eq!(
            cfg.endpoint.unwrap().url.to_string(),
            "unix://2f7661722f72756e2f64617461646f672f61706d2e736f636b6574/telemetry/proxy/api/v2/apmtelemetry"
        );
    }

    #[test]
    fn test_agent_host_detection_fallback() {
        let settings = Settings {
            trace_agent_url: None,
            agent_host: None,
            trace_agent_port: None,
            trace_pipe_name: None,
            agent_uds_socket_found: false,
            ..Default::default()
        };

        let cfg = Config::from_settings(&settings);
        assert_eq!(
            cfg.endpoint.unwrap().url.to_string(),
            "http://localhost:8126/telemetry/proxy/api/v2/apmtelemetry"
        );
    }

    #[test]
    fn test_config_set_url() {
        let mut cfg = Config::default();

        cfg.set_host_from_url("http://example.com/any_path_will_be_ignored")
            .unwrap();

        assert_eq!(
            "http://example.com/telemetry/proxy/api/v2/apmtelemetry",
            cfg.clone().endpoint.unwrap().url
        );
    }

    #[test]
    fn test_config_set_url_file() {
        let cases = [
            ("file:///absolute/path", "/absolute/path"),
            ("file://./relative/path", "./relative/path"),
            ("file://relative/path", "relative/path"),
            (
                "file://c://temp//with space\\foo.json",
                "c://temp//with space\\foo.json",
            ),
        ];

        for (input, expected) in cases {
            let mut cfg = Config::default();
            cfg.set_host_from_url(input).unwrap();

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
                Path::new(expected),
                ddcommon_net1::decode_uri_path_in_authority(&cfg.endpoint.unwrap().url).unwrap(),
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn test_config_set_url_unix_socket() {
        let mut cfg = Config::default();

        cfg.set_host_from_url("unix:///compatiliby/path").unwrap();
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

    #[test]
    fn test_config_set_url_windows_pipe() {
        let mut cfg = Config::default();

        cfg.set_host_from_url("windows:C:\\system32\\foo").unwrap();
        assert_eq!(
            "windows://433a5c73797374656d33325c666f6f/telemetry/proxy/api/v2/apmtelemetry",
            cfg.clone().endpoint.unwrap().url.to_string()
        );
        assert_eq!(
            "C:\\system32\\foo",
            named_pipe::named_pipe_path_from_uri(&cfg.clone().endpoint.unwrap().url)
                .unwrap()
                .to_string_lossy()
        );
    }
}
