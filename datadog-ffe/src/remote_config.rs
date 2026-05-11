// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::rules_based::UniversalFlagConfig;
use datadog_remote_config::{RemoteConfigContent, RemoteConfigProduct};

impl RemoteConfigContent for UniversalFlagConfig {
    const PRODUCT: RemoteConfigProduct = RemoteConfigProduct::FfeFlags;

    fn parse(data: &[u8]) -> anyhow::Result<Self> {
        Ok(UniversalFlagConfig::from_json(data.to_vec())?)
    }
}
