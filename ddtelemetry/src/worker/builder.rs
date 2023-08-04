// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::time::Duration;

use crate::config::Config;
use ddcommon::Endpoint;

#[derive(Default, Debug)]
pub struct ConfigBuilder {
    pub endpoint: Option<Endpoint>,
    pub telemetry_debug_logging_enabled: Option<bool>,
    pub telemetry_hearbeat_interval: Option<Duration>,
}

impl ConfigBuilder {
    pub fn merge(self, other: Config) -> Config {
        Config {
            endpoint: self.endpoint.or(other.endpoint),
            telemetry_debug_logging_enabled: self
                .telemetry_debug_logging_enabled
                .unwrap_or(other.telemetry_debug_logging_enabled),
            telemetry_hearbeat_interval: self
                .telemetry_hearbeat_interval
                .unwrap_or(other.telemetry_hearbeat_interval),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_builder_merge() {
        let builder = ConfigBuilder {
            telemetry_debug_logging_enabled: Some(true),
            endpoint: None,
            telemetry_hearbeat_interval: None,
        };

        let merged = builder.merge(Config::default());

        assert!(merged.telemetry_debug_logging_enabled);
    }
}
