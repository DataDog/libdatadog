// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{
    config::{
        agent_config::{self, AgentConfigFile},
        agent_task::{self, AgentTaskFile},
        dynamic::{self, DynamicConfigFile},
    },
    RemoteConfigPath, RemoteConfigProduct, RemoteConfigSource,
};
use std::any::Any;
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter, Result};

/// Opaque parsed payload of a remote config product. Implemented by every type that impls
/// [`RemoteConfigContent`]; product crates do not implement this trait directly.
///
/// Use `parsed.as_any().downcast_ref::<T>()` to recover the concrete product type.
pub trait RemoteConfigParsedData: Any + Debug + Send + Sync + 'static {
    fn as_any(&self) -> &dyn Any;
}
impl<T: RemoteConfigContent> RemoteConfigParsedData for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Error returned by [`RemoteConfigContent::parse`].
///
/// JSON failures convert via `?`; product-specific failures with their own
/// error types should box through [`ParseError::Custom`] rather than
/// collapsing to a string, so callers can downcast if they need to.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Custom(Box<dyn std::error::Error + Send + Sync>),
}

/// Typed contract a product crate provides so the registry can build a parser for [`Self`].
pub trait RemoteConfigContent: Any + Debug + Send + Sync + 'static {
    const PRODUCT: RemoteConfigProduct;
    fn parse(data: &[u8]) -> std::result::Result<Self, ParseError>
    where
        Self: Sized;
}

/// A product-specific parser: converts raw bytes into a parsed payload.
pub type ProductParser =
    Box<dyn Fn(&[u8]) -> anyhow::Result<Box<dyn RemoteConfigParsedData>> + Send + Sync>;

/// Returned by [`ParserRegistry::register`] when a parser is already registered for a product.
#[derive(Debug)]
pub struct AlreadyRegistered(pub RemoteConfigProduct);

impl Display for AlreadyRegistered {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        write!(f, "parser already registered for product {}", self.0)
    }
}

impl std::error::Error for AlreadyRegistered {}

/// Maps [`RemoteConfigProduct`] variants to their parser functions.
///
/// Build a registry (optionally starting from [`default_registry`]) and inject it into the file
/// storage or fetcher. Products with no registered parser yield `Ok(None)` so callers can still
/// track the config without processing its contents.
pub struct ParserRegistry {
    parsers: HashMap<RemoteConfigProduct, ProductParser>,
}

impl ParserRegistry {
    pub fn new() -> Self {
        ParserRegistry {
            parsers: HashMap::new(),
        }
    }

    /// Register `parser` for `product`. Errors if a parser is already registered for `product` —
    /// silent overwrites would mask configuration mistakes.
    pub fn register(
        &mut self,
        product: RemoteConfigProduct,
        parser: ProductParser,
    ) -> std::result::Result<(), AlreadyRegistered> {
        if self.parsers.contains_key(&product) {
            return Err(AlreadyRegistered(product));
        }
        self.parsers.insert(product, parser);
        Ok(())
    }

    /// Builder-style registration of a typed [`RemoteConfigContent`] implementor.
    /// Returns `Err(AlreadyRegistered)` if `T::PRODUCT` is already registered, so chains can
    /// propagate the collision instead of panicking.
    pub fn with<T: RemoteConfigContent>(
        mut self,
    ) -> std::result::Result<Self, AlreadyRegistered> {
        let parser: ProductParser = Box::new(|data: &[u8]| {
            let parsed = T::parse(data)?;
            Ok(Box::new(parsed) as Box<dyn RemoteConfigParsedData>)
        });
        self.register(T::PRODUCT, parser)?;
        Ok(self)
    }

    /// Parse `data` for `product`. Returns `Ok(None)` (not an error) when no parser is
    /// registered, so callers can still track the config in their bookkeeping structures.
    pub fn parse(
        &self,
        product: RemoteConfigProduct,
        data: &[u8],
    ) -> anyhow::Result<Option<Box<dyn RemoteConfigParsedData>>> {
        match self.parsers.get(&product) {
            Some(parser) => parser(data).map(Some),
            None => Ok(None),
        }
    }
}

impl Default for ParserRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ── RemoteConfigContent impls for RC-internal product types ───────────────────

impl RemoteConfigContent for AgentConfigFile {
    const PRODUCT: RemoteConfigProduct = RemoteConfigProduct::AgentConfig;

    fn parse(data: &[u8]) -> std::result::Result<Self, ParseError> {
        Ok(agent_config::parse_json(data)?)
    }
}

impl RemoteConfigContent for AgentTaskFile {
    const PRODUCT: RemoteConfigProduct = RemoteConfigProduct::AgentTask;

    fn parse(data: &[u8]) -> std::result::Result<Self, ParseError> {
        Ok(agent_task::parse_json(data)?)
    }
}

impl RemoteConfigContent for DynamicConfigFile {
    const PRODUCT: RemoteConfigProduct = RemoteConfigProduct::ApmTracing;

    fn parse(data: &[u8]) -> std::result::Result<Self, ParseError> {
        Ok(dynamic::parse_json(data)?)
    }
}

/// Returns a registry pre-loaded with parsers for the RC-internal products.
///
/// Consumers that need additional product parsers (live-debugger, FFE, …) should chain
/// [`ParserRegistry::with`] on the returned registry.
pub fn default_registry() -> ParserRegistry {
    fn build() -> std::result::Result<ParserRegistry, AlreadyRegistered> {
        ParserRegistry::new()
            .with::<AgentConfigFile>()?
            .with::<AgentTaskFile>()?
            .with::<DynamicConfigFile>()
    }
    #[allow(clippy::expect_used)]
    build().expect("default_registry: internal products are distinct by construction")
}

// ── RemoteConfigValue ─────────────────────────────────────────────────────────

pub struct RemoteConfigValue {
    pub source: RemoteConfigSource,
    pub product: RemoteConfigProduct,
    pub data: Option<Box<dyn RemoteConfigParsedData>>,
    pub config_id: String,
    pub name: String,
}

impl Debug for RemoteConfigValue {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        f.debug_struct("RemoteConfigValue")
            .field("source", &self.source)
            .field("product", &self.product)
            .field("config_id", &self.config_id)
            .field("name", &self.name)
            .finish()
    }
}

impl RemoteConfigValue {
    pub fn try_parse(path: &str, data: &[u8], registry: &ParserRegistry) -> anyhow::Result<Self> {
        let path = RemoteConfigPath::try_parse(path)?;
        let data = registry.parse(path.product, data)?;
        Ok(RemoteConfigValue {
            source: path.source,
            product: path.product,
            data,
            config_id: path.config_id.to_string(),
            name: path.name.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_parser() -> ProductParser {
        Box::new(|_data: &[u8]| anyhow::bail!("not invoked in this test"))
    }

    #[test]
    fn parse_returns_none_for_unregistered_product() {
        let registry = ParserRegistry::new();
        let parsed = registry
            .parse(RemoteConfigProduct::AsmFeatures, b"{}")
            .expect("parse must not error for unregistered products");
        assert!(parsed.is_none());
    }

    #[test]
    fn register_rejects_duplicate_product() {
        let mut registry = ParserRegistry::new();
        registry
            .register(RemoteConfigProduct::AgentTask, noop_parser())
            .expect("first registration succeeds");

        let err = registry
            .register(RemoteConfigProduct::AgentTask, noop_parser())
            .expect_err("second registration for the same product must fail");
        assert_eq!(err.0, RemoteConfigProduct::AgentTask);
    }
}
