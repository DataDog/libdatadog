// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::rules_based::UniversalFlagConfig;
use datadog_remote_config::{ProductParser, RemoteConfigParsedData, RemoteConfigProduct};
use std::any::Any;

impl RemoteConfigParsedData for UniversalFlagConfig {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn product(&self) -> RemoteConfigProduct {
        RemoteConfigProduct::FfeFlags
    }
}

pub fn ffe_parser() -> ProductParser {
    Box::new(|data: &[u8]| {
        let parsed = UniversalFlagConfig::from_json(data.to_vec())?;
        Ok(Box::new(parsed) as Box<dyn RemoteConfigParsedData>)
    })
}
