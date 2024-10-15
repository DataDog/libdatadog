// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Debug, Copy, Clone, Eq, Hash, PartialEq)]
pub enum RemoteConfigSource {
    Datadog(u64 /* org_id */),
    Employee,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum RemoteConfigProduct {
    ApmTracing,
    AsmData,
    Asm,
    AsmDD,
    AsmFeatures,
    LiveDebugger,
}

impl Display for RemoteConfigProduct {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let str = match self {
            RemoteConfigProduct::ApmTracing => "APM_TRACING",
            RemoteConfigProduct::LiveDebugger => "LIVE_DEBUGGING",
            RemoteConfigProduct::Asm => "ASM",
            RemoteConfigProduct::AsmDD => "ASM_DD",
            RemoteConfigProduct::AsmData => "ASM_DATA",
            RemoteConfigProduct::AsmFeatures => "ASM_FEATURES",
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

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub struct RemoteConfigPathRef<'a> {
    pub source: RemoteConfigSource,
    pub product: RemoteConfigProduct,
    pub config_id: &'a str,
    pub name: &'a str,
}

impl RemoteConfigPath {
    pub fn try_parse(path: &str) -> anyhow::Result<RemoteConfigPathRef> {
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
                "APM_TRACING" => RemoteConfigProduct::ApmTracing,
                "LIVE_DEBUGGING" => RemoteConfigProduct::LiveDebugger,
                "ASM" => RemoteConfigProduct::Asm,
                "ASM_DD" => RemoteConfigProduct::AsmDD,
                "ASM_DATA" => RemoteConfigProduct::AsmData,
                "ASM_FEATURES" => RemoteConfigProduct::AsmFeatures,
                product => anyhow::bail!("Unknown product {}", product),
            },
            config_id: parts[parts.len() - 2],
            name: parts[parts.len() - 1],
        })
    }
}

impl<'a> Display for RemoteConfigPathRef<'a> {
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

impl RemoteConfigPathType for RemoteConfigPath {
    fn source(&self) -> RemoteConfigSource {
        self.source
    }

    fn product(&self) -> RemoteConfigProduct {
        self.product
    }

    fn config_id(&self) -> &str {
        self.config_id.as_str()
    }

    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn to_owned(&self) -> RemoteConfigPath {
        self.clone()
    }
}

impl<'a> RemoteConfigPathType for &RemoteConfigPathRef<'a> {
    fn source(&self) -> RemoteConfigSource {
        self.source
    }

    fn product(&self) -> RemoteConfigProduct {
        self.product
    }

    fn config_id(&self) -> &'a str {
        self.config_id
    }

    fn name(&self) -> &'a str {
        self.name
    }

    fn to_owned(&self) -> RemoteConfigPath {
        (*self).into()
    }
}

impl<'a> RemoteConfigPathType for RemoteConfigPathRef<'a> {
    fn source(&self) -> RemoteConfigSource {
        self.source
    }

    fn product(&self) -> RemoteConfigProduct {
        self.product
    }

    fn config_id(&self) -> &'a str {
        self.config_id
    }

    fn name(&self) -> &'a str {
        self.name
    }

    fn to_owned(&self) -> RemoteConfigPath {
        self.into()
    }
}

pub trait RemoteConfigPathType {
    fn source(&self) -> RemoteConfigSource;
    fn product(&self) -> RemoteConfigProduct;
    fn config_id(&self) -> &str;
    fn name(&self) -> &str;
    fn to_owned(&self) -> RemoteConfigPath;
}

impl ToOwned for dyn RemoteConfigPathType + '_ {
    type Owned = RemoteConfigPath;

    fn to_owned(&self) -> Self::Owned {
        self.to_owned()
    }
}

impl<'a> Borrow<dyn RemoteConfigPathType + 'a> for RemoteConfigPath {
    fn borrow(&self) -> &(dyn RemoteConfigPathType + 'a) {
        self
    }
}

impl<'a> Borrow<dyn RemoteConfigPathType + 'a> for Arc<RemoteConfigPath> {
    fn borrow(&self) -> &(dyn RemoteConfigPathType + 'a) {
        &**self
    }
}

impl Hash for dyn RemoteConfigPathType + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.source().hash(state);
        self.product().hash(state);
        self.config_id().hash(state);
        self.name().hash(state);
    }
}

impl PartialEq for dyn RemoteConfigPathType + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.config_id() == other.config_id()
            && self.name() == other.name()
            && self.source() == other.source()
            && self.product() == other.product()
    }
}

impl Eq for dyn RemoteConfigPathType + '_ {}
