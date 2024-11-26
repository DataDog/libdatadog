// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use crate::config::Config;
use ddcommon_net1::Endpoint;

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
            direct_submission_enabled: other.direct_submission_enabled,
            restartable: other.restartable,
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
