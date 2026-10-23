// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A simple wrapper around `chrono`, so users don’t have to depend on it directly.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::value::StrDeserializer};

pub type Timestamp = DateTime<Utc>;

/// Timestamp policy shared by subject cutoffs and allocation activation.
#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub(crate) struct OptOutTimestamp(pub Timestamp);

impl OptOutTimestamp {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        // D005's surrounding-whitespace rule applies to opt-out timestamps only.
        // Other syntax retains the existing Chrono timestamp grammar.
        if value.trim() != value {
            return Err("invalid opt-out timestamp");
        }
        Timestamp::deserialize(StrDeserializer::<serde::de::value::Error>::new(value))
            .map(Self)
            .map_err(|_| "invalid opt-out timestamp")
    }
}

impl<'de> Deserialize<'de> for OptOutTimestamp {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

pub fn now() -> Timestamp {
    Utc::now()
}
