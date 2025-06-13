// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use arc_swap::ArcSwap;

pub const DATADOG_RATES_PAYLOAD_VERSION_HEADER: &str = "datadog-rates-payload-version";

/// `AgentResponse` structure holds agent response information upon successful request.
#[derive(Debug, PartialEq)]
pub enum AgentResponse {
    Unchanged,
    Changed { body: String },
}

#[derive(Debug)]
pub(crate) struct AgentResponsePayloadVersion {
    payload_version: ArcSwap<String>,
}

impl AgentResponsePayloadVersion {
    pub fn new() -> Self {
        AgentResponsePayloadVersion {
            payload_version: ArcSwap::new(Arc::new("0".to_string())),
        }
    }

    pub fn header_value(&self) -> String {
        let value = self.payload_version.load();
        value.to_string()
    }

    /// Checks if the response header has changed and updates the internal state if it has.
    ///
    /// Returns `true` if the value was updated, `false` if it was unchanged.
    pub fn check_and_update(&self, response_header: &str) -> bool {
        let payload_version = self.payload_version.load();
        if payload_version.as_str() == response_header {
            return false;
        }
        self.payload_version
            .store(Arc::new(response_header.to_owned()));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_and_update() {
        let version = AgentResponsePayloadVersion::new();

        // Initial state is empty
        assert_eq!(version.header_value(), "0");

        // First update returns true
        assert!(version.check_and_update("abc123"));
        assert_eq!(version.header_value(), "abc123");

        // Same value returns false
        assert!(!version.check_and_update("abc123"));
        assert_eq!(version.header_value(), "abc123");

        // Change to new version returns true
        assert!(version.check_and_update("xyz789"));
        assert_eq!(version.header_value(), "xyz789");
    }
}
