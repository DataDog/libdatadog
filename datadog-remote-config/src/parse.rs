// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "client")]
use crate::file_storage::ParseFile;
use crate::{
    config::{
        self, agent_config::AgentConfigFile, agent_task::AgentTaskFile, dynamic::DynamicConfigFile,
    },
    RemoteConfigPath, RemoteConfigProduct, RemoteConfigSource,
};

/// Parsed payload for the products owned by `datadog-remote-config`.
///
/// Consumers that only care about these products can use this enum directly via
/// [`BuiltinProductsParser`]. Consumers that need additional products should define their own
/// enum and [`ParseFile`] implementation.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum BuiltinProducts {
    AgentConfig(AgentConfigFile),
    AgentTask(AgentTaskFile),
    ApmTracing(DynamicConfigFile),
    Other(RemoteConfigProduct),
}

impl BuiltinProducts {
    pub fn product(&self) -> RemoteConfigProduct {
        match self {
            BuiltinProducts::AgentConfig(_) => RemoteConfigProduct::AgentConfig,
            BuiltinProducts::AgentTask(_) => RemoteConfigProduct::AgentTask,
            BuiltinProducts::ApmTracing(_) => RemoteConfigProduct::ApmTracing,
            BuiltinProducts::Other(p) => *p,
        }
    }

    pub fn try_parse(product: RemoteConfigProduct, data: &[u8]) -> anyhow::Result<BuiltinProducts> {
        Ok(match product {
            RemoteConfigProduct::AgentConfig => {
                BuiltinProducts::AgentConfig(config::agent_config::parse_json(data)?)
            }
            RemoteConfigProduct::AgentTask => {
                BuiltinProducts::AgentTask(config::agent_task::parse_json(data)?)
            }
            RemoteConfigProduct::ApmTracing => {
                BuiltinProducts::ApmTracing(config::dynamic::parse_json(data)?)
            }
            other => BuiltinProducts::Other(other),
        })
    }
}

/// [`ParseFile`] implementation for [`BuiltinProducts`]. Use this with [`RawFileStorage`] when
/// no extra products beyond the RC-internal set need parsing.
///
/// [`RawFileStorage`]: crate::file_storage::RawFileStorage
#[cfg(feature = "client")]
#[derive(Clone, Default)]
pub struct BuiltinProductsParser;

#[cfg(feature = "client")]
impl ParseFile for BuiltinProductsParser {
    type Parsed = anyhow::Result<BuiltinProducts>;

    fn parse(&self, path: &RemoteConfigPath, contents: Vec<u8>) -> Self::Parsed {
        BuiltinProducts::try_parse(path.product, &contents)
    }
}

/// A parsed remote config file along with metadata extracted from its path.
///
/// `T` is the consumer-defined parsed-payload enum; for the built-in set, use
/// [`BuiltinProducts`].
pub struct RemoteConfigValue<T> {
    pub source: RemoteConfigSource,
    pub data: T,
    pub config_id: String,
    pub name: String,
}

impl<T: std::fmt::Debug> std::fmt::Debug for RemoteConfigValue<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteConfigValue")
            .field("source", &self.source)
            .field("data", &self.data)
            .field("config_id", &self.config_id)
            .field("name", &self.name)
            .finish()
    }
}

impl<T> RemoteConfigValue<T> {
    pub fn try_parse(
        path: &str,
        data: &[u8],
        parse: impl FnOnce(RemoteConfigProduct, &[u8]) -> anyhow::Result<T>,
    ) -> anyhow::Result<Self> {
        let path = RemoteConfigPath::try_parse(path)?;
        let parsed = parse(path.product, data)?;
        Ok(RemoteConfigValue {
            source: path.source,
            data: parsed,
            config_id: path.config_id.to_string(),
            name: path.name.to_string(),
        })
    }
}
