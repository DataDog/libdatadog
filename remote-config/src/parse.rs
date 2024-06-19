// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{RemoteConfigPath, RemoteConfigProduct, RemoteConfigSource};
use datadog_live_debugger::LiveDebuggingData;
use datadog_dynamic_configuration::data::DynamicConfigFile;

#[derive(Debug)]
pub enum RemoteConfigData {
    DynamicConfig(DynamicConfigFile),
    LiveDebugger(LiveDebuggingData),
}

impl RemoteConfigData {
    pub fn try_parse(
        product: RemoteConfigProduct,
        data: &[u8],
    ) -> anyhow::Result<RemoteConfigData> {
        Ok(match product {
            RemoteConfigProduct::ApmTracing => {
                RemoteConfigData::DynamicConfig(datadog_dynamic_configuration::parse_json(data)?)
            }
            RemoteConfigProduct::LiveDebugger => {
                let parsed = datadog_live_debugger::parse_json(&String::from_utf8_lossy(data))?;
                RemoteConfigData::LiveDebugger(parsed)
            }
        })
    }
}

impl From<&RemoteConfigData> for RemoteConfigProduct {
    fn from(value: &RemoteConfigData) -> Self {
        match value {
            RemoteConfigData::DynamicConfig(_) => RemoteConfigProduct::ApmTracing,
            RemoteConfigData::LiveDebugger(_) => RemoteConfigProduct::LiveDebugger,
        }
    }
}

#[derive(Debug)]
pub struct RemoteConfigValue {
    pub source: RemoteConfigSource,
    pub data: RemoteConfigData,
    pub config_id: String,
    pub name: String,
}

impl RemoteConfigValue {
    pub fn try_parse(path: &str, data: &[u8]) -> anyhow::Result<Self> {
        let path = RemoteConfigPath::try_parse(path)?;
        let data = RemoteConfigData::try_parse(path.product, data)?;
        Ok(RemoteConfigValue {
            source: path.source,
            data,
            config_id: path.config_id.to_string(),
            name: path.name.to_string(),
        })
    }
}
