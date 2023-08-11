// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use datadog_live_debugger::LiveDebuggingData;

pub enum RemoteConfigSource {
    Datadog(u64 /* org_id */),
    Employee,
}

pub enum RemoteConfigProduct {
    LiveDebugger,
}

pub struct RemoteConfigPath {
    pub source: RemoteConfigSource,
    pub product: RemoteConfigProduct,
    pub config_id: String,
    pub name: String,
}

impl RemoteConfigPath {
    pub fn try_parse(path: &str) -> Option<Self> {
        let parts: Vec<_> = path.split("/").collect();
        Some(RemoteConfigPath {
            source: match parts[0] {
                "datadog" => {
                    if parts.len() != 5 {
                        return None;
                    }
                    RemoteConfigSource::Datadog(parts[1].parse().ok()?)
                }
                "employee" => {
                    if parts.len() != 4 {
                        return None;
                    }
                    RemoteConfigSource::Employee
                }
                _ => return None,
            },
            product: match parts[parts.len() - 3] {
                "LIVE_DEBUGGING" => RemoteConfigProduct::LiveDebugger,
                _ => return None,
            },
            config_id: parts[parts.len() - 2].to_string(),
            name: parts[parts.len() - 1].to_string(),
        })
    }
}

pub enum RemoteConfigData {
    LiveDebugger(LiveDebuggingData),
}

impl RemoteConfigData {
    pub fn try_parse(product: RemoteConfigProduct, data: &[u8]) -> Option<RemoteConfigData> {
        match product {
            RemoteConfigProduct::LiveDebugger => {
                if let Ok(parsed) =
                    datadog_live_debugger::parse_json(&String::from_utf8_lossy(data))
                {
                    Some(RemoteConfigData::LiveDebugger(parsed))
                } else {
                    None
                }
            }
        }
    }
}

pub struct RemoteConfigValue {
    pub source: RemoteConfigSource,
    pub data: RemoteConfigData,
    pub config_id: String,
    pub name: String,
}

impl RemoteConfigValue {
    pub fn try_parse(path: &str, data: &[u8]) -> Option<Self> {
        RemoteConfigPath::try_parse(path).and_then(|path| {
            RemoteConfigData::try_parse(path.product, data).map(|data| RemoteConfigValue {
                source: path.source,
                data,
                config_id: path.config_id,
                name: path.name,
            })
        })
    }
}
