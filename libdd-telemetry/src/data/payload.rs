// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::data::*;
use serde::Serialize;

#[derive(Serialize, Debug)]
#[serde(tag = "request_type", content = "payload")]
#[serde(rename_all = "kebab-case")]
pub enum Payload {
    AppStarted(AppStarted),
    AppDependenciesLoaded(AppDependenciesLoaded),
    AppIntegrationsChange(AppIntegrationsChange),
    AppClientConfigurationChange(AppClientConfigurationChange),
    AppEndpoints(AppEndpoints),
    AppHeartbeat(#[serde(skip_serializing)] ()),
    AppClosing(#[serde(skip_serializing)] ()),
    GenerateMetrics(GenerateMetrics),
    Sketches(Distributions),
    Logs(Logs),
    MessageBatch(Vec<Payload>),
    AppExtendedHeartbeat(AppStarted),
}

impl Payload {
    pub fn request_type(&self) -> &'static str {
        use Payload::*;
        match self {
            AppStarted(_) => "app-started",
            AppDependenciesLoaded(_) => "app-dependencies-loaded",
            AppIntegrationsChange(_) => "app-integrations-change",
            AppClientConfigurationChange(_) => "app-client-configuration-change",
            AppEndpoints(_) => "app-endpoints",
            AppHeartbeat(_) => "app-heartbeat",
            AppClosing(_) => "app-closing",
            GenerateMetrics(_) => "generate-metrics",
            Sketches(_) => "sketches",
            Logs(_) => "logs",
            MessageBatch(_) => "message-batch",
            AppExtendedHeartbeat(_) => "app-extended-heartbeat",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_app_started_serialization() {
        let payload = Payload::AppStarted(AppStarted {
            configuration: vec![
                Configuration {
                    name: "sampling_rate".to_string(),
                    value: "0.5".to_string(),
                    origin: ConfigurationOrigin::EnvVar,
                    config_id: Some("config-123".to_string()),
                    seq_id: Some(42),
                },
                Configuration {
                    name: "log_level".to_string(),
                    value: "debug".to_string(),
                    origin: ConfigurationOrigin::Code,
                    config_id: None,
                    seq_id: None,
                },
            ],
        });

        let serialized = serde_json::to_value(&payload).unwrap();

        let expected = json!({
            "request_type": "app-started",
            "payload": {
                "configuration": [
                    {
                        "name": "sampling_rate",
                        "value": "0.5",
                        "origin": "env_var",
                        "config_id": "config-123",
                        "seq_id": 42
                    },
                    {
                        "name": "log_level",
                        "value": "debug",
                        "origin": "code",
                        "config_id": null,
                        "seq_id": null
                    }
                ]
            }
        });

        assert_eq!(serialized, expected);
    }

    #[test]
    fn test_app_dependencies_loaded_serialization() {
        let payload = Payload::AppDependenciesLoaded(AppDependenciesLoaded {
            dependencies: vec![
                Dependency {
                    name: "tokio".to_string(),
                    version: Some("1.32.0".to_string()),
                },
                Dependency {
                    name: "serde".to_string(),
                    version: None,
                },
            ],
        });

        let serialized = serde_json::to_value(&payload).unwrap();

        let expected = json!({
            "request_type": "app-dependencies-loaded",
            "payload": {
                "dependencies": [
                    {
                        "name": "tokio",
                        "version": "1.32.0"
                    },
                    {
                        "name": "serde",
                        "version": null
                    }
                ]
            }
        });

        assert_eq!(serialized, expected);
    }

    #[test]
    fn test_app_integrations_change_serialization() {
        let payload = Payload::AppIntegrationsChange(AppIntegrationsChange {
            integrations: vec![
                Integration {
                    name: "postgres".to_string(),
                    enabled: true,
                    version: Some("0.19.0".to_string()),
                    compatible: Some(true),
                    auto_enabled: Some(false),
                },
                Integration {
                    name: "redis".to_string(),
                    enabled: false,
                    version: None,
                    compatible: None,
                    auto_enabled: None,
                },
            ],
        });

        let serialized = serde_json::to_value(&payload).unwrap();

        let expected = json!({
            "request_type": "app-integrations-change",
            "payload": {
                "integrations": [
                    {
                        "name": "postgres",
                        "enabled": true,
                        "version": "0.19.0",
                        "compatible": true,
                        "auto_enabled": false
                    },
                    {
                        "name": "redis",
                        "enabled": false,
                        "version": null,
                        "compatible": null,
                        "auto_enabled": null
                    }
                ]
            }
        });

        assert_eq!(serialized, expected);
    }

    #[test]
    fn test_app_client_configuration_change_serialization() {
        let payload = Payload::AppClientConfigurationChange(AppClientConfigurationChange {
            configuration: vec![Configuration {
                name: "timeout".to_string(),
                value: "30s".to_string(),
                origin: ConfigurationOrigin::RemoteConfig,
                config_id: Some("remote-1".to_string()),
                seq_id: Some(10),
            }],
        });

        let serialized = serde_json::to_value(&payload).unwrap();

        let expected = json!({
            "request_type": "app-client-configuration-change",
            "payload": {
                "configuration": [
                    {
                        "name": "timeout",
                        "value": "30s",
                        "origin": "remote_config",
                        "config_id": "remote-1",
                        "seq_id": 10
                    }
                ]
            }
        });

        assert_eq!(serialized, expected);
    }

    #[test]
    fn test_app_endpoints_serialization() {
        let payload = Payload::AppEndpoints(AppEndpoints {
            is_first: true,
            endpoints: vec![
                json!({
                    "method": "GET",
                    "path": "/api/users",
                    "operation_name": "get_users",
                    "resource_name": "users"
                }),
                json!({
                    "method": "POST",
                    "path": "/api/users",
                    "operation_name": "create_user",
                    "resource_name": "users"
                }),
            ],
        });

        let serialized = serde_json::to_value(&payload).unwrap();

        let expected = json!({
            "request_type": "app-endpoints",
            "payload": {
                "is_first": true,
                "endpoints": [
                    {
                        "method": "GET",
                        "path": "/api/users",
                        "operation_name": "get_users",
                        "resource_name": "users"
                    },
                    {
                        "method": "POST",
                        "path": "/api/users",
                        "operation_name": "create_user",
                        "resource_name": "users"
                    }
                ]
            }
        });

        assert_eq!(serialized, expected);
    }

    #[test]
    fn test_app_heartbeat_serialization() {
        let payload = Payload::AppHeartbeat(());

        let serialized = serde_json::to_value(&payload).unwrap();

        let expected = json!({
            "request_type": "app-heartbeat"
        });

        assert_eq!(serialized, expected);
    }

    #[test]
    fn test_app_closing_serialization() {
        let payload = Payload::AppClosing(());

        let serialized = serde_json::to_value(&payload).unwrap();

        let expected = json!({
            "request_type": "app-closing"
        });

        assert_eq!(serialized, expected);
    }

    #[test]
    fn test_generate_metrics_serialization() {
        let payload = Payload::GenerateMetrics(GenerateMetrics {
            series: vec![
                metrics::Serie {
                    namespace: metrics::MetricNamespace::Tracers,
                    metric: "spans_created".to_string(),
                    points: vec![(1234567890, 42.0), (1234567900, 43.0)],
                    tags: vec![],
                    common: true,
                    _type: metrics::MetricType::Count,
                    interval: 10,
                },
                metrics::Serie {
                    namespace: metrics::MetricNamespace::Profilers,
                    metric: "cpu_time".to_string(),
                    points: vec![(1234567890, 0.75)],
                    tags: vec![],
                    common: false,
                    _type: metrics::MetricType::Gauge,
                    interval: 60,
                },
            ],
        });

        let serialized = serde_json::to_value(&payload).unwrap();

        let expected = json!({
            "request_type": "generate-metrics",
            "payload": {
                "series": [
                    {
                        "namespace": "tracers",
                        "metric": "spans_created",
                        "points": [[1234567890, 42.0], [1234567900, 43.0]],
                        "tags": [],
                        "common": true,
                        "type": "count",
                        "interval": 10
                    },
                    {
                        "namespace": "profilers",
                        "metric": "cpu_time",
                        "points": [[1234567890, 0.75]],
                        "tags": [],
                        "common": false,
                        "type": "gauge",
                        "interval": 60
                    }
                ]
            }
        });

        assert_eq!(serialized, expected);
    }

    #[test]
    fn test_sketches_serialization() {
        let payload = Payload::Sketches(Distributions {
            series: vec![metrics::Distribution {
                namespace: metrics::MetricNamespace::Tracers,
                metric: "request_duration".to_string(),
                tags: vec![],
                sketch: metrics::SerializedSketch::B64 {
                    sketch_b64: "base64encodeddata".to_string(),
                },
                common: true,
                interval: 10,
                _type: metrics::MetricType::Distribution,
            }],
        });

        let serialized = serde_json::to_value(&payload).unwrap();

        let expected = json!({
            "request_type": "sketches",
            "payload": {
                "series": [
                    {
                        "namespace": "tracers",
                        "metric": "request_duration",
                        "tags": [],
                        "sketch_b64": "base64encodeddata",
                        "common": true,
                        "interval": 10,
                        "type": "distribution"
                    }
                ]
            }
        });

        assert_eq!(serialized, expected);
    }

    #[test]
    fn test_logs_serialization() {
        let payload = Payload::Logs(Logs {
            logs: vec![
                Log {
                    message: "Connection error".to_string(),
                    level: LogLevel::Error,
                    count: 1,
                    stack_trace: Some("at main.rs:42".to_string()),
                    tags: "env:prod".to_string(),
                    is_sensitive: false,
                    is_crash: false,
                },
                Log {
                    message: "Deprecated function used".to_string(),
                    level: LogLevel::Warn,
                    count: 5,
                    stack_trace: None,
                    tags: String::new(),
                    is_sensitive: false,
                    is_crash: false,
                },
            ],
        });

        let serialized = serde_json::to_value(&payload).unwrap();

        let expected = json!({
            "request_type": "logs",
            "payload": {
                "logs": [
                    {
                        "message": "Connection error",
                        "level": "ERROR",
                        "count": 1,
                        "stack_trace": "at main.rs:42",
                        "tags": "env:prod",
                        "is_sensitive": false,
                        "is_crash": false
                    },
                    {
                        "message": "Deprecated function used",
                        "level": "WARN",
                        "count": 5,
                        "stack_trace": null,
                        "tags": "",
                        "is_sensitive": false,
                        "is_crash": false
                    }
                ]
            }
        });

        assert_eq!(serialized, expected);
    }

    #[test]
    fn test_message_batch_serialization() {
        let payload = Payload::MessageBatch(vec![
            Payload::AppHeartbeat(()),
            Payload::Logs(Logs {
                logs: vec![Log {
                    message: "Test log".to_string(),
                    level: LogLevel::Debug,
                    count: 1,
                    stack_trace: None,
                    tags: String::new(),
                    is_sensitive: false,
                    is_crash: false,
                }],
            }),
        ]);

        let serialized = serde_json::to_value(&payload).unwrap();

        let expected = json!({
            "request_type": "message-batch",
            "payload": [
                {
                    "request_type": "app-heartbeat"
                },
                {
                    "request_type": "logs",
                    "payload": {
                        "logs": [
                            {
                                "message": "Test log",
                                "level": "DEBUG",
                                "count": 1,
                                "stack_trace": null,
                                "tags": "",
                                "is_sensitive": false,
                                "is_crash": false
                            }
                        ]
                    }
                }
            ]
        });

        assert_eq!(serialized, expected);
    }

    #[test]
    fn test_app_extended_heartbeat_serialization() {
        let payload = Payload::AppExtendedHeartbeat(AppStarted {
            configuration: vec![Configuration {
                name: "feature_flag".to_string(),
                value: "enabled".to_string(),
                origin: ConfigurationOrigin::Default,
                config_id: None,
                seq_id: None,
            }],
        });

        let serialized = serde_json::to_value(&payload).unwrap();

        let expected = json!({
            "request_type": "app-extended-heartbeat",
            "payload": {
                "configuration": [
                    {
                        "name": "feature_flag",
                        "value": "enabled",
                        "origin": "default",
                        "config_id": null,
                        "seq_id": null
                    }
                ]
            }
        });

        assert_eq!(serialized, expected);
    }
}
