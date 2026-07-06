// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::hash::Hash;
use std::sync::Arc;

#[derive(Debug, Copy, Clone, Eq, Hash, PartialEq)]
pub enum RemoteConfigSource {
    Datadog(u64 /* org_id */),
    Employee,
}

#[repr(C)]
#[derive(
    Debug,
    Copy,
    Clone,
    Eq,
    Hash,
    PartialEq,
    Serialize,
    Deserialize,
    strum_macros::EnumIter,
    strum_macros::IntoStaticStr,
)]
pub enum RemoteConfigProduct {
    AgentConfig,
    AgentTask,
    ApmTracing,
    Asm,
    AsmData,
    AsmDD,
    AsmFeatures,
    FfeFlags,
    LiveDebugger,
    LiveDebuggerSymbolDb,
}

impl Display for RemoteConfigProduct {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let str = match self {
            RemoteConfigProduct::AgentConfig => "AGENT_CONFIG",
            RemoteConfigProduct::AgentTask => "AGENT_TASK",
            RemoteConfigProduct::ApmTracing => "APM_TRACING",
            RemoteConfigProduct::Asm => "ASM",
            RemoteConfigProduct::AsmData => "ASM_DATA",
            RemoteConfigProduct::AsmDD => "ASM_DD",
            RemoteConfigProduct::AsmFeatures => "ASM_FEATURES",
            RemoteConfigProduct::FfeFlags => "FFE_FLAGS",
            RemoteConfigProduct::LiveDebugger => "LIVE_DEBUGGING",
            RemoteConfigProduct::LiveDebuggerSymbolDb => "LIVE_DEBUGGING_SYMBOL_DB",
        };
        write!(f, "{str}")
    }
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub struct RemoteConfigPath {
    pub source: RemoteConfigSource,
    pub product: RemoteConfigProduct,
    pub config_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub struct RemoteConfigPathRef<'a> {
    pub source: RemoteConfigSource,
    pub product: RemoteConfigProduct,
    pub config_id: &'a str,
    pub name: &'a str,
}

impl RemoteConfigPath {
    pub fn try_parse(path: &str) -> anyhow::Result<RemoteConfigPathRef<'_>> {
        let parts: Vec<_> = path.split('/').collect();
        Ok(RemoteConfigPathRef {
            source: match parts[0] {
                "datadog" => {
                    if parts.len() != 5 {
                        anyhow::bail!("{} is datadog and does not have exactly 5 parts", path);
                    }
                    RemoteConfigSource::Datadog(parts[1].parse()?)
                }
                "employee" => {
                    if parts.len() != 4 {
                        anyhow::bail!("{} is employee and does not have exactly 4 parts", path);
                    }
                    RemoteConfigSource::Employee
                }
                source => anyhow::bail!("Unknown source {}", source),
            },
            product: match parts[parts.len() - 3] {
                "AGENT_CONFIG" => RemoteConfigProduct::AgentConfig,
                "AGENT_TASK" => RemoteConfigProduct::AgentTask,
                "APM_TRACING" => RemoteConfigProduct::ApmTracing,
                "ASM" => RemoteConfigProduct::Asm,
                "ASM_DATA" => RemoteConfigProduct::AsmData,
                "ASM_DD" => RemoteConfigProduct::AsmDD,
                "ASM_FEATURES" => RemoteConfigProduct::AsmFeatures,
                "FFE_FLAGS" => RemoteConfigProduct::FfeFlags,
                "LIVE_DEBUGGING" => RemoteConfigProduct::LiveDebugger,
                "LIVE_DEBUGGING_SYMBOL_DB" => RemoteConfigProduct::LiveDebuggerSymbolDb,
                product => anyhow::bail!("Unknown product {}", product),
            },
            config_id: parts[parts.len() - 2],
            name: parts[parts.len() - 1],
        })
    }
}

impl Display for RemoteConfigPathRef<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
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

impl Display for RemoteConfigPath {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        RemoteConfigPathRef::from(self).fmt(f)
    }
}

impl<'a> From<&RemoteConfigPathRef<'a>> for RemoteConfigPath {
    fn from(from: &RemoteConfigPathRef<'a>) -> RemoteConfigPath {
        RemoteConfigPath {
            source: from.source,
            product: from.product,
            config_id: from.config_id.to_owned(),
            name: from.name.to_owned(),
        }
    }
}
impl<'a> From<RemoteConfigPathRef<'a>> for RemoteConfigPath {
    fn from(from: RemoteConfigPathRef<'a>) -> RemoteConfigPath {
        (&from).into()
    }
}

impl<'a> From<&'a RemoteConfigPath> for RemoteConfigPathRef<'a> {
    fn from(from: &'a RemoteConfigPath) -> RemoteConfigPathRef<'a> {
        RemoteConfigPathRef {
            source: from.source,
            product: from.product,
            config_id: from.config_id.as_str(),
            name: from.name.as_str(),
        }
    }
}

impl<'a> hashbrown::Equivalent<Arc<RemoteConfigPath>> for RemoteConfigPathRef<'a> {
    fn equivalent(&self, key: &Arc<RemoteConfigPath>) -> bool {
        let RemoteConfigPathRef {
            source,
            product,
            config_id,
            name,
        } = self;
        source == &key.source
            && product == &key.product
            && config_id == &key.config_id
            && name == &key.name
    }
}
