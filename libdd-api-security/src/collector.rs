// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::configuration::{object, string};
use crate::{
    ConfigurationDiagnostics, ConfigurationError, ProcessorDefinition, ProcessorOverride, Scanner,
    ScannerReference, Schema, extract_schema, extract_schema_with_scanners,
    load_processor_overrides, load_processors, load_scanners,
};
use alloc::collections::BTreeMap;
use semver::Version;
use serde::Serialize;
use serde_json::Value;

/// Diagnostics for one supplied native configuration section.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum SectionDiagnostics {
    Items(ConfigurationDiagnostics),
    Error { error: String },
}

/// Native-format diagnostics for collection sections only.
#[derive(Debug, Default, Serialize)]
pub struct CollectionDiagnostics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ruleset_version: Option<String>,
    #[serde(flatten)]
    pub sections: BTreeMap<String, SectionDiagnostics>,
}

#[derive(Default)]
struct Configuration {
    scanners: BTreeMap<String, Scanner>,
    processors: BTreeMap<String, ProcessorDefinition>,
    overrides: Vec<ProcessorOverride>,
}

fn section_error(input: &Value, error: &ConfigurationError) -> SectionDiagnostics {
    // Preserve the native container diagnostic. Per-item diagnostics retain
    // typed parser errors and allow valid neighboring items to load.
    let obtained = match input {
        Value::Object(_) => "map",
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => {
            return SectionDiagnostics::Error {
                error: error.to_string(),
            };
        }
    };
    SectionDiagnostics::Error {
        error: format!("bad cast, expected 'array', obtained '{obtained}'"),
    }
}

/// Caller-owned configuration sources. Replacing/removing a source affects
/// future snapshots only. Regexes are compiled at update time and reused.
///
/// Only schema processors, scanners and their overrides are consumed.
/// Sampling, remote configuration transport and request state stay with callers.
#[derive(Default)]
pub struct CollectionBuilder {
    configurations: BTreeMap<String, Configuration>,
}

impl CollectionBuilder {
    /// Replace one complete source, returning diagnostics. Invalid items are
    /// omitted, just as in native configuration loading. Unrelated WAF fields
    /// are ignored. Source names and scanner IDs use deterministic lexical order.
    ///
    /// # Errors
    /// An invalid root or metadata field leaves the previous source untouched.
    pub fn update(
        &mut self,
        name: String,
        input: &Value,
        version: &Version,
    ) -> Result<CollectionDiagnostics, ConfigurationError> {
        let root = object(input, "configuration")?;
        let mut diagnostics = CollectionDiagnostics::default();
        if let Some(metadata) = root.get("metadata")
            && let Some(version) = object(metadata, "metadata")?.get("rules_version")
        {
            diagnostics.ruleset_version = Some(string(version, "rules_version")?.into());
        }
        let mut configuration = Configuration::default();
        if let Some(input) = root.get("scanners") {
            let result = match load_scanners(input, version) {
                Ok(result) => {
                    configuration.scanners = result.scanners;
                    SectionDiagnostics::Items(result.diagnostics)
                }
                Err(error) => section_error(input, &error),
            };
            diagnostics.sections.insert("scanners".into(), result);
        }
        if let Some(input) = root.get("processors") {
            let result = match load_processors(input, version) {
                Ok(result) => {
                    configuration.processors = result.processors;
                    SectionDiagnostics::Items(result.diagnostics)
                }
                Err(error) => section_error(input, &error),
            };
            diagnostics.sections.insert("processors".into(), result);
        }
        if let Some(input) = root.get("processor_overrides") {
            let result = match load_processor_overrides(input) {
                Ok(result) => {
                    configuration.overrides = result.overrides;
                    SectionDiagnostics::Items(result.diagnostics)
                }
                Err(error) => section_error(input, &error),
            };
            diagnostics
                .sections
                .insert("processor_overrides".into(), result);
        }
        self.configurations.insert(name, configuration);
        Ok(diagnostics)
    }

    /// Remove a source from future snapshots; existing collectors stay valid.
    pub fn remove(&mut self, name: &str) -> bool {
        self.configurations.remove(name).is_some()
    }

    /// Resolve scanner references and overrides once, off the request path.
    #[must_use]
    pub fn build(&self) -> Collector {
        let mut scanners = BTreeMap::new();
        let mut processors = BTreeMap::new();
        for config in self.configurations.values() {
            for (id, scanner) in &config.scanners {
                scanners.entry(id).or_insert(scanner);
            }
            for (id, processor) in &config.processors {
                processors.entry(id).or_insert(processor);
            }
        }
        let processors = processors
            .into_iter()
            .map(|(id, processor)| {
                let mut definition = processor.clone();
                for override_ in self.configurations.values().flat_map(|c| &c.overrides) {
                    if override_.targets.iter().any(
                        |target| matches!(target, ScannerReference::Id(target) if target == id),
                    ) {
                        definition
                            .scanners
                            .include
                            .extend_from_slice(&override_.scanners.include);
                        definition
                            .scanners
                            .exclude
                            .extend_from_slice(&override_.scanners.exclude);
                    }
                }
                let selected = scanners
                    .values()
                    .filter(|scanner| definition.scanners.includes(scanner))
                    .map(|scanner| (*scanner).clone())
                    .collect();
                CompiledProcessor {
                    definition,
                    scanners: selected,
                }
            })
            .collect();
        Collector { processors }
    }
}

struct CompiledProcessor {
    definition: ProcessorDefinition,
    scanners: Vec<Scanner>,
}

/// An immutable collection snapshot. No threads, global state, sampling or
/// WAF evaluation is performed. Old snapshots remain usable after updates.
pub struct Collector {
    processors: Vec<CompiledProcessor>,
}

/// A generated schema and its native routing flags. It is generated once even
/// when both evaluation and attribute output are requested.
pub struct CollectedSchema<'a> {
    pub address: &'a str,
    pub schema: Schema<'a>,
    pub evaluate: bool,
    pub output: bool,
}

impl Collector {
    /// Extract schemas from caller-owned address values.
    ///
    /// The lookup controls persistence and ephemeral overlays. Each call is
    /// independent; no request values or prior condition results are retained.
    /// The returned schemas borrow their keys and scanner tags.
    pub fn collect<'a>(
        &'a self,
        lookup: impl Fn(&str) -> Option<&'a Value>,
    ) -> Vec<CollectedSchema<'a>> {
        let mut collected = Vec::new();
        for processor in &self.processors {
            let definition = &processor.definition;
            if !definition
                .conditions
                .iter()
                .all(|condition| condition.matches(&lookup))
            {
                continue;
            }
            for mapping in &definition.mappings {
                if let Some(input) = mapping.input.resolve(&lookup) {
                    let schema = if processor.scanners.is_empty() {
                        extract_schema(input)
                    } else {
                        extract_schema_with_scanners(input, &processor.scanners)
                    };
                    collected.push(CollectedSchema {
                        address: &mapping.output,
                        schema,
                        evaluate: definition.evaluate,
                        output: definition.output,
                    });
                }
            }
        }
        collected
    }
}
