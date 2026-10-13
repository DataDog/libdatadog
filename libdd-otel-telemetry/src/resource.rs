// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use opentelemetry::{Key, KeyValue, Value};
use opentelemetry_sdk::Resource;

/// Builds an OTel `Resource` from primitive attributes, with Datadog's precedence rules.
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
    pub fn with_attribute<K, V>(mut self, key: K, value: V) -> Self
    where
        K: Into<Key>,
        V: Into<Value>,
    {
        self.attributes.push(KeyValue::new(key, value));
        self
    }

    pub(crate) fn build(self) -> Resource {
        let mut builder = Resource::builder();
        for attribute in self.attributes {
            builder = builder.with_attribute(attribute);
        }
        if let Some(service) = self.service {
            builder = builder.with_service_name(service);
        }
        if let Some(env) = self.env {
            builder =
                builder.with_attribute(opentelemetry::KeyValue::new("deployment.environment", env));
        }
        if let Some(version) = self.version {
            builder =
                builder.with_attribute(opentelemetry::KeyValue::new("service.version", version));
        }
        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry::{Array, Key, Value};

    use super::ResourceBuilder;

    #[test]
    fn preserves_opentelemetry_attribute_types() {
        let resource = ResourceBuilder::new()
            .with_attribute("bool", true)
            .with_attribute("int", 42_i64)
            .with_attribute("float", 1.5_f64)
            .with_attribute("string", "value")
            .with_attribute("bools", Value::Array(Array::Bool(vec![true, false])))
            .with_attribute("ints", Value::Array(Array::I64(vec![1, 2])))
            .with_attribute("floats", Value::Array(Array::F64(vec![1.5, 2.5])))
            .with_attribute(
                "strings",
                Value::Array(Array::String(vec!["a".into(), "b".into()])),
            )
            .build();

        assert_eq!(resource.get(&Key::new("bool")), Some(Value::Bool(true)));
        assert_eq!(resource.get(&Key::new("int")), Some(Value::I64(42)));
        assert_eq!(resource.get(&Key::new("float")), Some(Value::F64(1.5)));
        assert_eq!(
            resource.get(&Key::new("string")),
            Some(Value::String("value".into()))
        );
        assert_eq!(
            resource.get(&Key::new("bools")),
            Some(Value::Array(Array::Bool(vec![true, false])))
        );
        assert_eq!(
            resource.get(&Key::new("ints")),
            Some(Value::Array(Array::I64(vec![1, 2])))
        );
        assert_eq!(
            resource.get(&Key::new("floats")),
            Some(Value::Array(Array::F64(vec![1.5, 2.5])))
        );
        assert_eq!(
            resource.get(&Key::new("strings")),
            Some(Value::Array(Array::String(vec!["a".into(), "b".into()])))
        );
    }
}
