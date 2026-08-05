// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

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
    attributes: Vec<(String, String)>,
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
    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.push((key.into(), value.into()));
        self
    }

    pub fn build(self) -> Resource {
        let mut builder = Resource::builder();
        for (key, value) in self.attributes {
            builder = builder.with_attribute(opentelemetry::KeyValue::new(key, value));
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
