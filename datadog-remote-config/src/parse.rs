// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{
    config::{
        self, agent_config::AgentConfigFile, agent_task::AgentTaskFile, dynamic::DynamicConfigFile,
    },
    RemoteConfigPath, RemoteConfigProduct, RemoteConfigSource,
};
#[cfg(feature = "live-debugger")]
use libdd_live_debugger::LiveDebuggingData;

#[derive(Debug)]
pub enum RemoteConfigData {
    DynamicConfig(DynamicConfigFile),
    #[cfg(feature = "live-debugger")]
    LiveDebugger(LiveDebuggingData),
    TracerFlareConfig(AgentConfigFile),
    TracerFlareTask(AgentTaskFile),
    Ignored(RemoteConfigProduct),
}

impl RemoteConfigData {
    pub fn try_parse(
        product: RemoteConfigProduct,
        data: &[u8],
    ) -> anyhow::Result<RemoteConfigData> {
        Ok(match product {
            RemoteConfigProduct::AgentConfig => {
                RemoteConfigData::TracerFlareConfig(config::agent_config::parse_json(data)?)
            }
            RemoteConfigProduct::AgentTask => {
                RemoteConfigData::TracerFlareTask(config::agent_task::parse_json(data)?)
            }
            RemoteConfigProduct::ApmTracing => {
                RemoteConfigData::DynamicConfig(config::dynamic::parse_json(data)?)
            }
            #[cfg(feature = "live-debugger")]
            RemoteConfigProduct::LiveDebugger => {
                let parsed = libdd_live_debugger::parse_json(&String::from_utf8_lossy(data))?;
                RemoteConfigData::LiveDebugger(parsed)
            }
            _ => RemoteConfigData::Ignored(product),
        })
    }
}

impl From<&RemoteConfigData> for RemoteConfigProduct {
    fn from(value: &RemoteConfigData) -> Self {
        match value {
            RemoteConfigData::DynamicConfig(_) => RemoteConfigProduct::ApmTracing,
            #[cfg(feature = "live-debugger")]
            RemoteConfigData::LiveDebugger(_) => RemoteConfigProduct::LiveDebugger,
            RemoteConfigData::TracerFlareConfig(_) => RemoteConfigProduct::AgentConfig,
            RemoteConfigData::TracerFlareTask(_) => RemoteConfigProduct::AgentTask,
            RemoteConfigData::Ignored(product) => *product,
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
