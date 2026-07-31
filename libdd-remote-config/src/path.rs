// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
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

#[derive(Clone)]
pub struct RemoteConfigPath {
    raw: Box<str>,
    source: RemoteConfigSource,
    product: RemoteConfigProduct,
    /// Byte offset in `raw` where the `config_id` segment starts.
    /// The segment ends at `name_start - 1` (the `/` before `name`).
    config_id_start: u32,
    /// Byte offset in `raw` where the `name` segment starts. It runs to the
    /// end of `raw`.
    name_start: u32,
}

impl std::fmt::Debug for RemoteConfigPath {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteConfigPath")
            .field("source", &self.source)
            .field("product", &self.product)
            .field("config_id", &self.config_id())
            .field("name", &self.name())
            .finish()
    }
}

impl PartialEq for RemoteConfigPath {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}
impl Eq for RemoteConfigPath {}
impl Hash for RemoteConfigPath {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

#[derive(Debug, Copy, Clone)]
pub struct RemoteConfigPathRef<'a> {
    raw: &'a str,
    source: RemoteConfigSource,
    product: RemoteConfigProduct,
    config_id_start: u32,
    name_start: u32,
}

impl PartialEq for RemoteConfigPathRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}
impl Eq for RemoteConfigPathRef<'_> {}
impl Hash for RemoteConfigPathRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl RemoteConfigPath {
    pub fn try_parse(path: &str) -> anyhow::Result<RemoteConfigPathRef<'_>> {
        parse_into_ref(path)
    }

    pub fn parse(path: &str) -> anyhow::Result<Self> {
        let r = parse_into_ref(path)?;
        Ok(Self {
            raw: Box::from(r.raw),
            source: r.source,
            product: r.product,
            config_id_start: r.config_id_start,
            name_start: r.name_start,
        })
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    #[inline]
    pub fn source(&self) -> RemoteConfigSource {
        self.source
    }

    #[inline]
    pub fn product(&self) -> RemoteConfigProduct {
        self.product
    }

    #[inline]
    pub fn config_id(&self) -> &str {
        &self.raw[self.config_id_start as usize..self.name_start as usize - 1]
    }

    #[inline]
    pub fn name(&self) -> &str {
        &self.raw[self.name_start as usize..]
    }
}

impl<'a> RemoteConfigPathRef<'a> {
    #[inline]
    pub fn as_str(&self) -> &'a str {
        self.raw
    }

    #[inline]
    pub fn source(&self) -> RemoteConfigSource {
        self.source
    }

    #[inline]
    pub fn product(&self) -> RemoteConfigProduct {
        self.product
    }

    #[inline]
    pub fn config_id(&self) -> &'a str {
        &self.raw[self.config_id_start as usize..self.name_start as usize - 1]
    }

    #[inline]
    pub fn name(&self) -> &'a str {
        &self.raw[self.name_start as usize..]
    }
}

fn parse_into_ref(path: &str) -> anyhow::Result<RemoteConfigPathRef<'_>> {
    let slash_positions: Vec<usize> = path.match_indices('/').map(|(i, _)| i).collect();
    let n_slashes = slash_positions.len();

    // Every valid path has at least: source '/' ... '/' config_id '/' name.
    // Datadog paths have 4 slashes (5 segments); employee paths have 3.
    let first_slash = *slash_positions.first().ok_or_else(|| {
        anyhow::format_err!("path {path} does not contain a '/', cannot be a remote config path")
    })?;

    let source = match &path[..first_slash] {
        "datadog" => {
            if n_slashes != 4 {
                anyhow::bail!("{path} is datadog and does not have exactly 5 parts");
            }
            let org_id_end = slash_positions[1];
            let org_id: u64 = path[first_slash + 1..org_id_end].parse()?;
            // The agent parses org_id as an int64; reject values it would reject
            // (> i64::MAX) so both clients accept/reject the same paths.
            if org_id > i64::MAX as u64 {
                anyhow::bail!("org_id {org_id} exceeds i64::MAX in path {path}");
            }
            RemoteConfigSource::Datadog(org_id)
        }
        "employee" => {
            if n_slashes != 3 {
                anyhow::bail!("{path} is employee and does not have exactly 4 parts");
            }
            RemoteConfigSource::Employee
        }
        source => anyhow::bail!("Unknown source {source}"),
    };

    // Segments are indexed from the tail so both wire forms share this code.
    // `slash_positions[n_slashes - 3]` = '/' before product.
    // `slash_positions[n_slashes - 2]` = '/' before config_id.
    // `slash_positions[n_slashes - 1]` = '/' before name.
    let product_start = slash_positions[n_slashes - 3] + 1;
    let product_end = slash_positions[n_slashes - 2];
    let config_id_start = slash_positions[n_slashes - 2] + 1;
    let name_start = slash_positions[n_slashes - 1] + 1;

    let product = match &path[product_start..product_end] {
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
        product => anyhow::bail!("Unknown product {product}"),
    };

    if name_start == config_id_start + 1 {
        anyhow::bail!("empty config_id in path {path}");
    }
    if name_start >= path.len() {
        anyhow::bail!("empty name in path {path}");
    }

    let config_id_start_u32 = u32::try_from(config_id_start)
        .map_err(|_| anyhow::format_err!("path {path} is too long (>= 4 GiB)"))?;
    let name_start_u32 = u32::try_from(name_start)
        .map_err(|_| anyhow::format_err!("path {path} is too long (>= 4 GiB)"))?;

    Ok(RemoteConfigPathRef {
        raw: path,
        source,
        product,
        config_id_start: config_id_start_u32,
        name_start: name_start_u32,
    })
}

impl Display for RemoteConfigPathRef<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.raw)
    }
}

impl Display for RemoteConfigPath {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

impl<'a> From<&RemoteConfigPathRef<'a>> for RemoteConfigPath {
    fn from(from: &RemoteConfigPathRef<'a>) -> RemoteConfigPath {
        RemoteConfigPath {
            raw: Box::from(from.raw),
            source: from.source,
            product: from.product,
            config_id_start: from.config_id_start,
            name_start: from.name_start,
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
            raw: &from.raw,
            source: from.source,
            product: from.product,
            config_id_start: from.config_id_start,
            name_start: from.name_start,
        }
    }
}

impl<'a> hashbrown::Equivalent<Arc<RemoteConfigPath>> for RemoteConfigPathRef<'a> {
    fn equivalent(&self, key: &Arc<RemoteConfigPath>) -> bool {
        self.raw == key.raw.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_datadog_roundtrip() {
        let raw = "datadog/42/APM_TRACING/cfg-1/tracing.config";
        let p = RemoteConfigPath::parse(raw).unwrap();
        assert_eq!(p.as_str(), raw);
        assert_eq!(p.source(), RemoteConfigSource::Datadog(42));
        assert_eq!(p.product(), RemoteConfigProduct::ApmTracing);
        assert_eq!(p.config_id(), "cfg-1");
        assert_eq!(p.name(), "tracing.config");
        assert_eq!(p.to_string(), raw);
    }

    #[test]
    fn parse_employee_roundtrip() {
        let raw = "employee/ASM_DD/blocklist/rules";
        let p = RemoteConfigPath::parse(raw).unwrap();
        assert_eq!(p.as_str(), raw);
        assert_eq!(p.source(), RemoteConfigSource::Employee);
        assert_eq!(p.product(), RemoteConfigProduct::AsmDD);
        assert_eq!(p.config_id(), "blocklist");
        assert_eq!(p.name(), "rules");
        assert_eq!(p.to_string(), raw);
    }

    #[test]
    fn ref_matches_owned() {
        let raw = "datadog/1/ASM/cfg/name";
        let r = RemoteConfigPath::try_parse(raw).unwrap();
        let o: RemoteConfigPath = r.into();
        assert_eq!(o.as_str(), raw);
        assert_eq!(o.config_id(), "cfg");
        assert_eq!(o.name(), "name");
    }

    #[test]
    fn hash_and_eq_by_wire_form() {
        let a = RemoteConfigPath::parse("datadog/1/ASM/cfg/name").unwrap();
        let b = RemoteConfigPath::parse("datadog/1/ASM/cfg/name").unwrap();
        assert_eq!(a, b);
        use std::collections::HashSet;
        let mut s = HashSet::new();
        s.insert(a);
        assert!(s.contains(&b));
    }

    #[test]
    fn rejects_bad_paths() {
        assert!(RemoteConfigPath::parse("").is_err());
        assert!(RemoteConfigPath::parse("nosource/x/y/z").is_err());
        assert!(RemoteConfigPath::parse("datadog/1/APM_TRACING/cfg").is_err());
        assert!(RemoteConfigPath::parse("datadog/1/APM_TRACING//name").is_err());
        assert!(RemoteConfigPath::parse("datadog/1/APM_TRACING/cfg/").is_err());
        assert!(RemoteConfigPath::parse("datadog/notanint/APM_TRACING/cfg/name").is_err());
        // org_id > i64::MAX must be rejected to stay in lockstep with the
        // agent, which parses org_id as int64.
        assert!(
            RemoteConfigPath::parse("datadog/9223372036854775808/APM_TRACING/cfg/name").is_err()
        );
        assert!(RemoteConfigPath::parse("employee/UNKNOWN_PRODUCT/cfg/name").is_err());
    }
}
