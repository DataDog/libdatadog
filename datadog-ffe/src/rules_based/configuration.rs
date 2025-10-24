// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Cow;

use chrono::{DateTime, Utc};

use crate::rules_based::{ufc::UniversalFlagConfig, Str};

/// Remote configuration for the feature flagging client. It's a central piece that defines client
/// behavior.
#[derive(Debug)]
pub struct Configuration {
    /// Timestamp when configuration was fetched by the SDK.
    #[allow(dead_code)]
    pub(crate) fetched_at: DateTime<Utc>,
    /// Flags configuration.
    pub(crate) flags: UniversalFlagConfig,
}

impl Configuration {
    /// Create a new configuration from server responses.
    pub fn from_server_response(config: UniversalFlagConfig) -> Configuration {
        let now = Utc::now();

        Configuration {
            fetched_at: now,
            flags: config,
        }
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.flags.compiled.created_at
    }

    pub fn environment(&self) -> &str {
        &self.flags.compiled.environment.name
    }

    /// Returns an iterator over all flag keys. Note that this may return both disabled flags and
    /// flags with bad configuration. Mostly useful for debugging.
    pub fn flag_keys(&self) -> impl Iterator<Item = &Str> {
        self.flags.compiled.flags.keys()
    }

    /// Returns bytes representing flags configuration.
    ///
    /// The return value should be treated as opaque and passed on to another feature flagging
    /// client for initialization.
    pub fn get_flags_configuration(&self) -> Option<Cow<'_, [u8]>> {
        Some(Cow::Borrowed(self.flags.to_json()))
    }
}
