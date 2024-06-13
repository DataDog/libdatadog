// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::dynamic_configuration::data::DynamicConfigFile;
use serde::{Deserialize, Serialize};
use std::fmt::Display;

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub enum RemoteConfigSource {
    Datadog(u64 /* org_id */),
    Employee,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum RemoteConfigProduct {
    ApmTracing,
    LiveDebugger,
}

impl Display for RemoteConfigProduct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let str = match self {
            RemoteConfigProduct::ApmTracing => "APM_TRACING",
            RemoteConfigProduct::LiveDebugger => "LIVE_DEBUGGING",
        };
        write!(f, "{}", str)
    }
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
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

impl Display for RemoteConfigPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.source {
            RemoteConfigSource::Datadog(id) => write!(
                f,
                "datadog/{}/{}/{}/{}",
                id, self.product, self.config_id, self.name
            ),
            RemoteConfigSource::Employee => {
                write!(
                    f,
                    "employee/{}/{}/{}",
                    self.product, self.config_id, self.name
                )
            }
        }
    }
}

#[derive(Debug)]
pub enum RemoteConfigData {
    DynamicConfig(DynamicConfigFile),
    LiveDebugger(()),
}

impl RemoteConfigData {
    pub fn try_parse(
        product: RemoteConfigProduct,
        data: &[u8],
    ) -> anyhow::Result<RemoteConfigData> {
        Ok(match product {
            RemoteConfigProduct::ApmTracing => {
                RemoteConfigData::DynamicConfig(serde_json::from_slice(data)?)
            }
            RemoteConfigProduct::LiveDebugger => {
                RemoteConfigData::LiveDebugger(/* placeholder */ ())
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
            config_id: path.config_id,
            name: path.name,
        })
    }
}
