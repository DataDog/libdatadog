// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP trace export configuration.
//!
//! OTLP trace export is enabled when `OTEL_TRACES_EXPORTER=otlp` is set.
//! When enabled, endpoint, headers, timeout, and protocol are read from the
//! `OTEL_EXPORTER_OTLP_TRACES_*` (and generic `OTEL_EXPORTER_OTLP_*`) environment variables.

use std::env;
use std::time::Duration;

/// OTLP trace export protocol. Support for HTTP/JSON for now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OtlpProtocol {
    /// HTTP with JSON body (Content-Type: application/json). Default for HTTP.
    #[default]
    HttpJson,
    /// HTTP with protobuf body. (Not supported yet)
    HttpProtobuf,
    /// gRPC. (Not supported yet)
    Grpc,
}

impl OtlpProtocol {
    fn from_str(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "http/json" => OtlpProtocol::HttpJson,
            "http/protobuf" => OtlpProtocol::HttpProtobuf,
            "grpc" => OtlpProtocol::Grpc,
            _ => OtlpProtocol::HttpJson,
        }
    }
}

/// Default OTLP HTTP endpoint (no path; path /v1/traces is appended when building request URL).
pub const DEFAULT_OTLP_HTTP_ENDPOINT: &str = "http://localhost:4318";
/// Default OTLP gRPC endpoint.
pub const DEFAULT_OTLP_GRPC_ENDPOINT: &str = "http://localhost:4317";
/// OTLP traces path for HTTP.
pub const OTLP_TRACES_PATH: &str = "/v1/traces";

/// Parsed OTLP trace exporter configuration.
#[derive(Clone, Debug)]
pub struct OtlpTraceConfig {
    /// Full URL to POST traces (e.g. http://localhost:4318/v1/traces).
    pub endpoint_url: String,
    /// Optional HTTP headers (key-value pairs).
    pub headers: Vec<(String, String)>,
    /// Request timeout.
    pub timeout: Duration,
    /// Protocol (for future use; currently only HttpJson is supported).
    pub protocol: OtlpProtocol,
}

/// Environment variable names (standard OTEL and traces-specific).
pub mod env_keys {
    pub const TRACES_EXPORTER: &str = "OTEL_TRACES_EXPORTER";
    pub const TRACES_PROTOCOL: &str = "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL";
    pub const PROTOCOL: &str = "OTEL_EXPORTER_OTLP_PROTOCOL";
    pub const TRACES_ENDPOINT: &str = "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT";
    pub const ENDPOINT: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";
    pub const TRACES_HEADERS: &str = "OTEL_EXPORTER_OTLP_TRACES_HEADERS";
    pub const HEADERS: &str = "OTEL_EXPORTER_OTLP_HEADERS";
    pub const TRACES_TIMEOUT: &str = "OTEL_EXPORTER_OTLP_TRACES_TIMEOUT";
    pub const TIMEOUT: &str = "OTEL_EXPORTER_OTLP_TIMEOUT";
}

/// Default timeout for OTLP export (10 seconds).
const DEFAULT_OTLP_TIMEOUT_MS: u64 = 10_000;

fn get_env(key: &str) -> Option<String> {
    env::var(key).ok().filter(|s| !s.trim().is_empty())
}

/// Parse OTEL headers string "key1=value1,key2=value2" into a list of (key, value).
fn parse_headers(s: &str) -> Vec<(String, String)> {
    s.split(',')
        .filter_map(|pair| {
            let pair = pair.trim();
            let eq = pair.find('=')?;
            let key = pair[..eq].trim();
            let value = pair[eq + 1..].trim();
            if key.is_empty() {
                return None;
            }
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

/// Append /v1/traces to a base URL for HTTP trace export. Used only when the endpoint
/// is the fallback default (path is added only for fallback, not when user sets endpoint).
fn fallback_traces_url(base: &str, protocol: OtlpProtocol) -> String {
    let base = base.trim().trim_end_matches('/');
    match protocol {
        OtlpProtocol::HttpJson | OtlpProtocol::HttpProtobuf => {
            format!("{}{}", base, OTLP_TRACES_PATH)
        }
        OtlpProtocol::Grpc => base.to_string(),
    }
}

/// Resolve OTLP trace export configuration from environment.
///
/// **Enablement:** Returns `Some(config)` only when `OTEL_TRACES_EXPORTER=otlp` is set.
/// Returns `None` otherwise (use Datadog agent).
///
/// **Endpoint:** If `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (or generic
/// `OTEL_EXPORTER_OTLP_ENDPOINT`) is set, that value is used **as-is**. Otherwise the
/// fallback default is used and, for http/json or http/protobuf, the path `/v1/traces`
/// is appended.
/// **Precedence:** Traces-specific env vars override generic OTEL vars for protocol,
/// endpoint, headers, and timeout.
pub fn otlp_trace_config_from_env() -> Option<OtlpTraceConfig> {
    let exporter = get_env(env_keys::TRACES_EXPORTER)?;
    if exporter.trim().to_lowercase() != "otlp" {
        return None;
    }

    let protocol_str = get_env(env_keys::TRACES_PROTOCOL).or_else(|| get_env(env_keys::PROTOCOL));
    let protocol = protocol_str
        .as_deref()
        .map(OtlpProtocol::from_str)
        .unwrap_or_default();

    // Traces-specific endpoint takes precedence over generic OTEL endpoint when both are set.
    // Per spec: OTEL_EXPORTER_OTLP_TRACES_ENDPOINT is used as-is; the generic
    // OTEL_EXPORTER_OTLP_ENDPOINT gets /v1/traces appended for HTTP signals.
    let traces_endpoint = get_env(env_keys::TRACES_ENDPOINT);
    let (endpoint_opt, is_signal_specific) = match traces_endpoint {
        Some(ep) => (Some(ep), true),
        None => (get_env(env_keys::ENDPOINT), false),
    };
    let url = match endpoint_opt {
        Some(s) => {
            let endpoint = s.trim().to_string();
            if endpoint.is_empty() {
                fallback_traces_url(DEFAULT_OTLP_HTTP_ENDPOINT, protocol)
            } else {
                // Normalize bare host:port to a full URL.
                let normalized = if endpoint.contains("://") {
                    endpoint
                } else if endpoint.starts_with(':') {
                    format!("http://localhost{}", endpoint)
                } else {
                    format!("http://{}", endpoint)
                };
                // Spec: signal-specific TRACES_ENDPOINT is used as-is; generic ENDPOINT gets
                // /v1/traces appended for HTTP.
                if is_signal_specific {
                    normalized
                } else {
                    fallback_traces_url(&normalized, protocol)
                }
            }
        }
        None => fallback_traces_url(DEFAULT_OTLP_HTTP_ENDPOINT, protocol),
    };

    let headers_str = get_env(env_keys::TRACES_HEADERS).or_else(|| get_env(env_keys::HEADERS));
    let headers = headers_str
        .as_deref()
        .map(parse_headers)
        .unwrap_or_default();

    let timeout_ms = get_env(env_keys::TRACES_TIMEOUT)
        .or_else(|| get_env(env_keys::TIMEOUT))
        .and_then(|s| parse_timeout(&s))
        .unwrap_or(DEFAULT_OTLP_TIMEOUT_MS);

    Some(OtlpTraceConfig {
        endpoint_url: url,
        headers,
        timeout: Duration::from_millis(timeout_ms),
        protocol,
    })
}

/// Parse timeout string: digits with optional unit (ms, s, m). Default unit: milliseconds.
fn parse_timeout(s: &str) -> Option<u64> {
    let s = s.trim();
    let s = s.to_lowercase();
    if s.ends_with("ms") {
        s[..s.len() - 2].trim().parse::<u64>().ok()
    } else if s.ends_with('s') && !s.ends_with("ms") {
        s[..s.len() - 1]
            .trim()
            .parse::<u64>()
            .ok()
            .map(|v| v * 1000)
    } else if s.ends_with('m') {
        s[..s.len() - 1]
            .trim()
            .parse::<u64>()
            .ok()
            .map(|v| v * 60 * 1000)
    } else {
        s.parse::<u64>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Env-var-dependent tests must be serialized: parallel mutation of global env is not safe.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_parse_headers() {
        let h = parse_headers("key1=val1,key2=val2");
        assert_eq!(h.len(), 2);
        assert_eq!(h[0], ("key1".to_string(), "val1".to_string()));
        assert_eq!(h[1], ("key2".to_string(), "val2".to_string()));
    }

    #[test]
    fn test_parse_timeout() {
        assert_eq!(parse_timeout("5000"), Some(5000));
        assert_eq!(parse_timeout("5s"), Some(5000));
        assert_eq!(parse_timeout("100ms"), Some(100));
    }

    #[test]
    fn test_fallback_traces_url() {
        // Fallback: path /v1/traces is appended for http/json
        assert_eq!(
            fallback_traces_url("http://localhost:4318", OtlpProtocol::HttpJson),
            "http://localhost:4318/v1/traces"
        );
        assert_eq!(
            fallback_traces_url(DEFAULT_OTLP_HTTP_ENDPOINT, OtlpProtocol::HttpJson),
            "http://localhost:4318/v1/traces"
        );
    }

    #[test]
    fn test_protocol_from_str() {
        assert_eq!(OtlpProtocol::from_str("http/json"), OtlpProtocol::HttpJson);
        assert_eq!(OtlpProtocol::from_str("grpc"), OtlpProtocol::Grpc);
    }

    #[test]
    fn test_otlp_disabled_without_traces_exporter() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Without OTEL_TRACES_EXPORTER=otlp, config should be None
        std::env::remove_var(env_keys::TRACES_EXPORTER);
        std::env::remove_var(env_keys::TRACES_ENDPOINT);
        std::env::remove_var(env_keys::ENDPOINT);
        assert!(otlp_trace_config_from_env().is_none());
    }

    #[test]
    fn test_explicit_endpoint_used_as_is() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Per spec: when OTEL_EXPORTER_OTLP_TRACES_ENDPOINT is set, use as-is (no /v1/traces
        // appended)
        std::env::remove_var(env_keys::TRACES_EXPORTER);
        std::env::remove_var(env_keys::TRACES_ENDPOINT);
        std::env::remove_var(env_keys::ENDPOINT);
        std::env::set_var(env_keys::TRACES_EXPORTER, "otlp");
        std::env::set_var(env_keys::TRACES_ENDPOINT, "http://custom:9999");
        let config = otlp_trace_config_from_env();
        std::env::remove_var(env_keys::TRACES_EXPORTER);
        std::env::remove_var(env_keys::TRACES_ENDPOINT);
        let config = config.expect("config when TRACES_EXPORTER=otlp and endpoint set");
        assert_eq!(config.endpoint_url, "http://custom:9999");
    }

    #[test]
    fn test_generic_endpoint_gets_path_appended() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Per spec: OTEL_EXPORTER_OTLP_ENDPOINT (generic) must have /v1/traces appended for HTTP.
        std::env::remove_var(env_keys::TRACES_EXPORTER);
        std::env::remove_var(env_keys::TRACES_ENDPOINT);
        std::env::remove_var(env_keys::ENDPOINT);
        std::env::set_var(env_keys::TRACES_EXPORTER, "otlp");
        std::env::set_var(env_keys::ENDPOINT, "http://collector:4318");
        let config = otlp_trace_config_from_env();
        std::env::remove_var(env_keys::TRACES_EXPORTER);
        std::env::remove_var(env_keys::TRACES_ENDPOINT);
        std::env::remove_var(env_keys::ENDPOINT);
        let config = config.expect("config when TRACES_EXPORTER=otlp and generic endpoint set");
        assert_eq!(config.endpoint_url, "http://collector:4318/v1/traces");
    }
}
