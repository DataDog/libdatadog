// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::Deserialize;
use serde_json::{Map, Value};
use std::io::Error as IoError;
use std::io::ErrorKind as IoErrorKind;

use crate::trace_exporter::error::TraceExporterError;
use std::{f64, str::FromStr};

#[derive(Debug, Deserialize)]
pub struct Rates {
    rate_by_service: Map<String, Value>,
}

impl Rates {
    pub fn get(&self, service: &str, env: &str) -> Result<f64, IoError> {
        for (id, value) in &self.rate_by_service {
            let mut it = id
                .split(',')
                .filter_map(|pair| pair.split_once(':'))
                .map(|(_, value)| value);

            let srv_pair = (it.next().unwrap_or(""), it.next().unwrap_or(""));
            if srv_pair == (service, env) {
                return value
                    .as_f64()
                    .ok_or(IoError::from(IoErrorKind::InvalidData));
            }
        }
        // Return default
        if let Some(default) = self.rate_by_service.get("service:,env:") {
            default
                .as_f64()
                .ok_or(IoError::from(IoErrorKind::InvalidData))
        } else {
            Err(IoError::from(IoErrorKind::NotFound))
        }
    }
}

impl FromStr for Rates {
    type Err = TraceExporterError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let obj: Rates = serde_json::from_str(s)?;
        Ok(obj)
    }
}

#[derive(Debug, PartialEq)]
#[repr(C)]
pub struct AgentResponse(f64);

impl From<f64> for AgentResponse {
    fn from(value: f64) -> Self {
        AgentResponse(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_test() {
        let payload = r#"{
            "rate_by_service": {
                "service:foo,env:staging": 1.0,
                "service:foo,env:prod": 0.3,
                "service:,env:": 0.8
            }
        }"#;

        let rates: Rates = payload.parse().unwrap();

        assert_eq!(rates.rate_by_service.len(), 3);
        assert_eq!(rates.get("foo", "staging").unwrap(), 1.0);
        assert_eq!(rates.get("foo", "prod").unwrap(), 0.3);
        assert_eq!(rates.get("bar", "bar-env").unwrap(), 0.8);
    }

    #[test]
    fn parse_invalid_data_test() {
        let payload = r#"{
            "rate_by_service": {
                "service:foo,env:staging": "",
                "service:,env:": "" 
            }
        }"#;

        let rates: Rates = payload.parse().unwrap();

        assert_eq!(rates.rate_by_service.len(), 2);
        assert!(rates
            .get("foo", "staging")
            .is_err_and(|e| e.kind() == IoErrorKind::InvalidData));
        assert!(rates
            .get("bar", "staging")
            .is_err_and(|e| e.kind() == IoErrorKind::InvalidData));
    }

    #[test]
    fn parse_invalid_payload_test() {
        let payload = r#"{
            "invalid": {
                "service:foo,env:staging": "",
                "service:,env:": "" 
            }
        }"#;

        let res = payload.parse::<Rates>();

        assert!(res.is_err());
    }
}
