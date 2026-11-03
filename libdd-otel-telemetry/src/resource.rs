// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::attribute::normalize_attributes;
use crate::{AttributeValue, KeyValue};

/// Builds OTLP resource attributes from primitive values, with Datadog's precedence rules.
///
/// Mirrors `datadog-opentelemetry::otlp_utils::build_otel_resource`'s merge order: explicit
/// `service`/`env`/`version` win over generic attributes, which win over defaults. Kept
/// `Config`-agnostic — each consumer extracts primitives from its own configuration and passes
/// them in, rather than this crate reading any tracer-specific config type directly.
#[derive(Debug, Default)]
pub struct ResourceBuilder {
    service: Option<String>,
    env: Option<String>,
    version: Option<String>,
    attributes: Vec<KeyValue>,
}

impl ResourceBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_service(mut self, service: impl Into<String>) -> Self {
        self.service = Some(service.into());
        self
    }

    pub fn with_env(mut self, env: impl Into<String>) -> Self {
        self.env = Some(env.into());
        self
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }

    /// Adds a generic resource attribute. Later calls with the same key overwrite earlier ones;
    /// `service`/`env`/`version` always take precedence over attributes added this way,
    /// regardless of call order.
    pub fn with_attribute(
        mut self,
        key: impl Into<String>,
        value: impl Into<AttributeValue>,
    ) -> Self {
        self.attributes.push(KeyValue::new(key, value));
        self
    }

    pub(crate) fn build(mut self) -> Vec<KeyValue> {
        self.attributes.retain(|attribute| {
            !matches!(
                attribute.key.as_str(),
                "service.name" | "service.version" | "deployment.environment.name"
            )
        });
        if let Some(service) = self.service {
            self.attributes.push(KeyValue::new("service.name", service));
        }
        if let Some(env) = self.env {
            self.attributes
                .push(KeyValue::new("deployment.environment.name", env));
        }
        if let Some(version) = self.version {
            self.attributes
                .push(KeyValue::new("service.version", version));
        }
        normalize_attributes(&self.attributes)
    }
}

#[cfg(test)]
mod tests {
    use crate::{AttributeArray, AttributeValue};

    use super::ResourceBuilder;

    #[test]
    fn preserves_opentelemetry_attribute_types() {
        let resource = ResourceBuilder::new()
            .with_attribute("bool", true)
            .with_attribute("int", 42_i64)
            .with_attribute("float", 1.5_f64)
            .with_attribute("string", "value")
            .with_attribute("bools", AttributeArray::Bool(vec![true, false]))
            .with_attribute("ints", AttributeArray::I64(vec![1, 2]))
            .with_attribute("floats", AttributeArray::F64(vec![1.5, 2.5]))
            .with_attribute(
                "strings",
                AttributeArray::String(vec!["a".into(), "b".into()]),
            )
            .build();

        let value = |key: &str| {
            resource
                .iter()
                .find(|attribute| attribute.key == key)
                .map(|attribute| attribute.value.clone())
        };
        assert_eq!(value("bool"), Some(AttributeValue::Bool(true)));
        assert_eq!(value("int"), Some(AttributeValue::I64(42)));
        assert_eq!(value("float"), Some(AttributeValue::F64(1.5)));
        assert_eq!(
            value("string"),
            Some(AttributeValue::String("value".into()))
        );
        assert_eq!(
            value("bools"),
            Some(AttributeValue::Array(AttributeArray::Bool(vec![
                true, false
            ])))
        );
        assert_eq!(
            value("ints"),
            Some(AttributeValue::Array(AttributeArray::I64(vec![1, 2])))
        );
        assert_eq!(
            value("floats"),
            Some(AttributeValue::Array(AttributeArray::F64(vec![1.5, 2.5])))
        );
        assert_eq!(
            value("strings"),
            Some(AttributeValue::Array(AttributeArray::String(vec![
                "a".into(),
                "b".into()
            ])))
        );
    }

    #[test]
    fn explicit_service_fields_take_precedence() {
        let resource = ResourceBuilder::new()
            .with_attribute("service.name", "generic")
            .with_attribute("deployment.environment.name", "generic")
            .with_service("checkout")
            .with_env("prod")
            .build();

        assert!(resource.contains(&crate::KeyValue::new("service.name", "checkout")));
        assert!(resource.contains(&crate::KeyValue::new("deployment.environment.name", "prod")));
    }
}
