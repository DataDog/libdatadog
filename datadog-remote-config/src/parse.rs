// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{
    config::{
        self, agent_config::AgentConfigFile, agent_task::AgentTaskFile, dynamic::DynamicConfigFile,
    },
    RemoteConfigPath, RemoteConfigProduct, RemoteConfigSource,
};
use std::any::Any;
use std::collections::HashMap;

/// Opaque parsed payload for a remote config product. Product crates implement this on their own
/// types and export a [`ProductParser`] factory; the RC crate stores and distributes the results
/// without knowing their concrete type.
pub trait RemoteConfigParsedData: Send + Sync + 'static {
    fn as_any(&self) -> &dyn Any;
    fn product(&self) -> RemoteConfigProduct;
}

/// A product-specific parser: converts raw bytes into a parsed payload.
pub type ProductParser =
    Box<dyn Fn(&[u8]) -> anyhow::Result<Box<dyn RemoteConfigParsedData>> + Send + Sync>;

/// Maps [`RemoteConfigProduct`] variants to their parser functions.
///
/// Consumers build a registry (optionally starting from [`default_registry`]) and inject it into
/// the file storage or fetcher. Products with no registered parser produce a parse error, which
/// is treated as "ignored" by callers that only care about specific products.
pub struct ParserRegistry {
    parsers: HashMap<RemoteConfigProduct, ProductParser>,
}

impl ParserRegistry {
    pub fn new() -> Self {
        ParserRegistry {
            parsers: HashMap::new(),
        }
    }

    /// Register a parser for a product. Replaces any existing parser for that product.
    pub fn register(&mut self, product: RemoteConfigProduct, parser: ProductParser) {
        self.parsers.insert(product, parser);
    }

    pub fn parse(
        &self,
        product: RemoteConfigProduct,
        data: &[u8],
    ) -> anyhow::Result<Box<dyn RemoteConfigParsedData>> {
        match self.parsers.get(&product) {
            Some(parser) => parser(data),
            None => anyhow::bail!("no parser registered for product {:?}", product),
        }
    }
}

impl Default for ParserRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── Implementations for RC-internal product types ────────────────────────────

impl RemoteConfigParsedData for DynamicConfigFile {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn product(&self) -> RemoteConfigProduct {
        RemoteConfigProduct::ApmTracing
    }
}

impl RemoteConfigParsedData for AgentConfigFile {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn product(&self) -> RemoteConfigProduct {
        RemoteConfigProduct::AgentConfig
    }
}

impl RemoteConfigParsedData for AgentTaskFile {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn product(&self) -> RemoteConfigProduct {
        RemoteConfigProduct::AgentTask
    }
}

/// Returns a registry pre-loaded with parsers for the RC-internal products:
/// [`RemoteConfigProduct::AgentConfig`], [`RemoteConfigProduct::AgentTask`], and
/// [`RemoteConfigProduct::ApmTracing`].
///
/// Consumers that need additional product parsers (live-debugger, FFE, …) should call
/// [`ParserRegistry::register`] on the returned registry before use.
pub fn default_registry() -> ParserRegistry {
    let mut registry = ParserRegistry::new();
    registry.register(
        RemoteConfigProduct::AgentConfig,
        Box::new(|data: &[u8]| {
            let parsed = config::agent_config::parse_json(data)?;
            Ok(Box::new(parsed) as Box<dyn RemoteConfigParsedData>)
        }),
    );
    registry.register(
        RemoteConfigProduct::AgentTask,
        Box::new(|data: &[u8]| {
            let parsed = config::agent_task::parse_json(data)?;
            Ok(Box::new(parsed) as Box<dyn RemoteConfigParsedData>)
        }),
    );
    registry.register(
        RemoteConfigProduct::ApmTracing,
        Box::new(|data: &[u8]| {
            let parsed = config::dynamic::parse_json(data)?;
            Ok(Box::new(parsed) as Box<dyn RemoteConfigParsedData>)
        }),
    );
    registry
}

// ── RemoteConfigValue ─────────────────────────────────────────────────────────

pub struct RemoteConfigValue {
    pub source: RemoteConfigSource,
    pub data: Box<dyn RemoteConfigParsedData>,
    pub config_id: String,
    pub name: String,
}

impl std::fmt::Debug for RemoteConfigValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteConfigValue")
            .field("source", &self.source)
            .field("product", &self.data.product())
            .field("config_id", &self.config_id)
            .field("name", &self.name)
            .finish()
    }
}

impl RemoteConfigValue {
    pub fn try_parse(
        path: &str,
        data: &[u8],
        registry: &ParserRegistry,
    ) -> anyhow::Result<Self> {
        let path = RemoteConfigPath::try_parse(path)?;
        let data = registry.parse(path.product, data)?;
        Ok(RemoteConfigValue {
            source: path.source,
            data,
            config_id: path.config_id.to_string(),
            name: path.name.to_string(),
        })
    }
}
