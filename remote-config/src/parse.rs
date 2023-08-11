// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use datadog_live_debugger::LiveDebuggingData;
use crate::dynamic_configuration::data::DynamicConfigFile;

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub enum RemoteConfigSource {
    Datadog(u64 /* org_id */),
    Employee,
}

#[derive(Copy, Clone, Eq, Hash, PartialEq)]
pub enum RemoteConfigProduct {
    ApmTracing,
    LiveDebugger,
}

impl ToString for RemoteConfigProduct {
    fn to_string(&self) -> String {
        match self {
            RemoteConfigProduct::ApmTracing => "APM_TRACING",
            RemoteConfigProduct::LiveDebugger => "LIVE_DEBUGGING",
        }.to_string()
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct RemoteConfigPath {
    pub source: RemoteConfigSource,
    pub product: RemoteConfigProduct,
    pub config_id: String,
    pub name: String,
}

impl RemoteConfigPath {
    pub fn try_parse(path: &str) -> anyhow::Result<Self> {
        let parts: Vec<_> = path.split('/').collect();
        Ok(RemoteConfigPath {
            source: match parts[0] {
                "datadog" => {
                    if parts.len() != 5 {
                        anyhow::bail!("{} is datadog and does not have exactly 5 parts", path);
                    }
                    RemoteConfigSource::Datadog(parts[1].parse()?)
                }
                "employee" => {
                    if parts.len() != 4 {
                        anyhow::bail!("{} is employee and does not have exactly 5 parts", path);
                    }
                    RemoteConfigSource::Employee
                }
                source => anyhow::bail!("Unknown source {}", source),
            },
            product: match parts[parts.len() - 3] {
                "APM_TRACING" => RemoteConfigProduct::ApmTracing,
                "LIVE_DEBUGGING" => RemoteConfigProduct::LiveDebugger,
                product => anyhow::bail!("Unknown product {}", product),
            },
            config_id: parts[parts.len() - 2].to_string(),
            name: parts[parts.len() - 1].to_string(),
        })
    }
}

impl ToString for RemoteConfigPath {
    fn to_string(&self) -> String {
        match self.source {
            RemoteConfigSource::Datadog(id) => format!("datadog/{}/{}/{}/{}", id, self.product.to_string(), self.config_id, self.name),
            RemoteConfigSource::Employee => format!("employee/{}/{}/{}", self.product.to_string(), self.config_id, self.name),
        }
    }
}

#[derive(Debug)]
pub enum RemoteConfigData {
    DynamicConfig(DynamicConfigFile),
    LiveDebugger(LiveDebuggingData),
}

impl RemoteConfigData {
    pub fn try_parse(product: RemoteConfigProduct, data: &[u8]) -> anyhow::Result<RemoteConfigData> {
        Ok(match product {
            RemoteConfigProduct::ApmTracing => {
                RemoteConfigData::DynamicConfig(serde_json::from_slice(data)?)
            },
            RemoteConfigProduct::LiveDebugger => {
                let parsed = datadog_live_debugger::parse_json(&String::from_utf8_lossy(data))?;
                RemoteConfigData::LiveDebugger(parsed)
            }
        })
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
            config_id: path.config_id,
            name: path.name,
        })
    }
}
