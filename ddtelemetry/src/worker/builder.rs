// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::path::PathBuf;

use crate::Config;
use ddcommon::Endpoint;

#[derive(Default, Debug)]
pub struct ConfigBuilder {
    pub endpoint: Option<Endpoint>,
    pub mock_client_file: Option<PathBuf>,
    pub telemetry_debug_logging_enabled: Option<bool>,
}

impl ConfigBuilder {
    pub fn merge(self, other: Config) -> Config {
        Config {
            endpoint: self.endpoint.or(other.endpoint),
            mock_client_file: self.mock_client_file.or(other.mock_client_file),
            telemetry_debug_logging_enabled: self
                .telemetry_debug_logging_enabled
                .unwrap_or(other.telemetry_debug_logging_enabled),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FromEnv;

    #[test]
    fn test_config_builder_merge() {
        let builder = ConfigBuilder {
            mock_client_file: Some(PathBuf::new()),
            telemetry_debug_logging_enabled: Some(true),
            endpoint: Some(FromEnv::build_endpoint("http://example.com", None).unwrap()),
        };

        let default_cfg = Config {
            endpoint: None,
            mock_client_file: None,
            telemetry_debug_logging_enabled: false,
        };

        let merged = builder.merge(default_cfg);

        assert!(merged.endpoint.is_some());
        assert!(merged.telemetry_debug_logging_enabled);
        assert!(merged.mock_client_file.is_some());
    }
}
