// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::configuration::{compatible, invalid, object, required, string};
use crate::{ConfigurationDiagnostics, ConfigurationError, Scanner, ScannerTags};
use alloc::collections::BTreeMap;
use semver::Version;
use serde_json::{Map, Value};

/// One component of an input's key path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyPath {
    /// A record property.
    Key(String),
    /// An array index. Negative indexes never resolve.
    Index(i64),
}

/// An address supplied by the caller and an optional path within it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputAddress {
    pub address: String,
    pub key_path: Vec<KeyPath>,
}

impl InputAddress {
    pub fn resolve<'a>(&self, lookup: &impl Fn(&str) -> Option<&'a Value>) -> Option<&'a Value> {
        let mut value = lookup(&self.address)?;
        for part in &self.key_path {
            value = match part {
                KeyPath::Key(key) => value.as_object()?.get(key)?,
                KeyPath::Index(index) => value.as_array()?.get(usize::try_from(*index).ok()?)?,
            };
        }
        Some(value)
    }
}

/// A schema input/output mapping. Like libddwaf, only the first input is used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessorMapping {
    pub input: InputAddress,
    pub output: String,
}

/// A scanner selection by ID or by all supplied tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScannerReference {
    Id(String),
    Tags(ScannerTags),
    /// Empty references do not match.
    None,
}

impl ScannerReference {
    fn matches(&self, scanner: &Scanner) -> bool {
        match self {
            Self::Id(id) => id == scanner.id(),
            Self::Tags(tags) => tags
                .iter()
                .all(|(key, value)| scanner.tags().get(key) == Some(value)),
            Self::None => false,
        }
    }
}

/// Inclusion and exclusion references, resolved once when building a collector.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScannerSelection {
    pub include: Vec<ScannerReference>,
    pub exclude: Vec<ScannerReference>,
}

impl ScannerSelection {
    #[must_use]
    pub fn includes(&self, scanner: &Scanner) -> bool {
        let matches_id =
            |r: &ScannerReference| matches!(r, ScannerReference::Id(_)) && r.matches(scanner);
        if self.exclude.iter().any(matches_id) {
            return false;
        }
        self.include.iter().any(matches_id)
            || (!self.exclude.iter().any(|r| r.matches(scanner))
                && self.include.iter().any(|r| r.matches(scanner)))
    }
}

/// One typed equality gate. Every gate must pass; any input may satisfy a gate.
#[derive(Clone, Debug)]
pub struct ProcessorCondition {
    pub inputs: Vec<InputAddress>,
    pub expected: Value,
}

impl ProcessorCondition {
    pub fn matches<'a>(&self, lookup: &impl Fn(&str) -> Option<&'a Value>) -> bool {
        self.inputs
            .iter()
            .any(|input| input.resolve(lookup) == Some(&self.expected))
    }
}

/// Native schema processor configuration, without any WAF rule/action state.
#[derive(Clone, Debug)]
pub struct ProcessorDefinition {
    pub conditions: Vec<ProcessorCondition>,
    pub mappings: Vec<ProcessorMapping>,
    pub scanners: ScannerSelection,
    /// Expose generated schemas to the caller for evaluation.
    pub evaluate: bool,
    /// Expose generated schemas as output attributes.
    pub output: bool,
}

/// Valid processor definitions and diagnostics for every supplied item.
#[derive(Debug, Default)]
pub struct ProcessorConfiguration {
    pub processors: BTreeMap<String, ProcessorDefinition>,
    pub diagnostics: ConfigurationDiagnostics,
}

/// Scanner overrides target processor IDs; overrides accumulate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessorOverride {
    pub targets: Vec<ScannerReference>,
    pub scanners: ScannerSelection,
}

/// Valid overrides and diagnostics for every supplied item.
#[derive(Debug, Default)]
pub struct OverrideConfiguration {
    pub overrides: Vec<ProcessorOverride>,
    pub diagnostics: ConfigurationDiagnostics,
}

fn array<'a>(value: &'a Value, field: &str) -> Result<&'a [Value], ConfigurationError> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| invalid(field, "array"))
}

fn optional_array<'a>(
    value: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a [Value], ConfigurationError> {
    value
        .get(field)
        .map(|v| array(v, field))
        .transpose()
        .map(Option::unwrap_or_default)
}

fn boolean(
    value: &Map<String, Value>,
    field: &str,
    default: bool,
) -> Result<bool, ConfigurationError> {
    value
        .get(field)
        .map(|v| v.as_bool().ok_or_else(|| invalid(field, "boolean")))
        .transpose()
        .map(|v| v.unwrap_or(default))
}

fn input(value: &Value) -> Result<InputAddress, ConfigurationError> {
    let value = object(value, "input")?;
    let address = string(required(value, "address")?, "address")?.into();
    let key_path = optional_array(value, "key_path")?
        .iter()
        .map(|v| {
            if let Some(key) = v.as_str() {
                if key.is_empty() {
                    return Err(ConfigurationError::InvalidDefinition("empty key_path"));
                }
                Ok(KeyPath::Key(key.into()))
            } else {
                v.as_i64()
                    .map(KeyPath::Index)
                    .ok_or_else(|| invalid("key_path", "string or integer"))
            }
        })
        .collect::<Result<_, _>>()?;
    Ok(InputAddress { address, key_path })
}

fn reference(value: &Value) -> Result<ScannerReference, ConfigurationError> {
    let value = object(value, "reference")?;
    for field in ["rule_id", "id"] {
        if let Some(id) = value.get(field) {
            let id = string(id, field)?;
            if !id.is_empty() {
                return Ok(ScannerReference::Id(id.into()));
            }
        }
    }
    if let Some(tags) = value.get("tags") {
        let tags = object(tags, "tags")?;
        if !tags.is_empty() {
            return tags
                .iter()
                .map(|(k, v)| Ok((k.clone(), string(v, k)?.into())))
                .collect::<Result<_, ConfigurationError>>()
                .map(ScannerReference::Tags);
        }
    }
    Ok(ScannerReference::None)
}

fn references(
    value: &Map<String, Value>,
    field: &str,
) -> Result<Vec<ScannerReference>, ConfigurationError> {
    optional_array(value, field)?
        .iter()
        .map(reference)
        .collect()
}

fn condition(value: &Value) -> Result<ProcessorCondition, ConfigurationError> {
    let value = object(value, "condition")?;
    let operator = string(required(value, "operator")?, "operator")?;
    if operator != "equals" {
        return Err(ConfigurationError::UnknownOperator(operator.into()));
    }
    let params = object(required(value, "parameters")?, "parameters")?;
    let expected = required(params, "value")?;
    let kind = string(required(params, "type")?, "type")?;
    let valid = match kind {
        "boolean" => expected.is_boolean(),
        "string" => expected.is_string(),
        "signed" => expected.as_i64().is_some(),
        "unsigned" => expected.as_u64().is_some(),
        "float" => expected.as_f64().is_some(),
        _ => false,
    };
    if !valid {
        return Err(ConfigurationError::InvalidDefinition(
            "unsupported or mismatched equality value type",
        ));
    }
    let inputs = array(required(params, "inputs")?, "inputs")?
        .iter()
        .map(input)
        .collect::<Result<Vec<_>, _>>()?;
    if inputs.is_empty() {
        return Err(ConfigurationError::InvalidDefinition(
            "empty condition inputs",
        ));
    }
    Ok(ProcessorCondition {
        inputs,
        expected: expected.clone(),
    })
}

fn processor(value: &Map<String, Value>) -> Result<ProcessorDefinition, ConfigurationError> {
    let generator = string(required(value, "generator")?, "generator")?;
    if generator != "extract_schema" {
        return Err(ConfigurationError::UnknownGenerator(generator.into()));
    }
    let conditions = optional_array(value, "conditions")?
        .iter()
        .map(condition)
        .collect::<Result<_, _>>()?;
    let params = object(required(value, "parameters")?, "parameters")?;
    let mappings = array(required(params, "mappings")?, "mappings")?;
    if mappings.is_empty() {
        return Err(ConfigurationError::InvalidDefinition("empty mappings"));
    }
    let mappings = mappings
        .iter()
        .map(|v| {
            let v = object(v, "mapping")?;
            let first = array(required(v, "inputs")?, "inputs")?.first().ok_or(
                ConfigurationError::InvalidDefinition("empty processor input mapping"),
            )?;
            Ok(ProcessorMapping {
                input: input(first)?,
                output: string(required(v, "output")?, "output")?.into(),
            })
        })
        .collect::<Result<_, ConfigurationError>>()?;
    let scanners = ScannerSelection {
        include: references(params, "scanners")?,
        exclude: Vec::new(),
    };
    let evaluate = boolean(value, "evaluate", true)?;
    let output = boolean(value, "output", false)?;
    if !evaluate && !output {
        return Err(ConfigurationError::InvalidDefinition(
            "processor not used for evaluation or output",
        ));
    }
    Ok(ProcessorDefinition {
        conditions,
        mappings,
        scanners,
        evaluate,
        output,
    })
}

/// Parse native schema processors. Unsupported generators/conditions are diagnosed.
///
/// # Errors
/// Returns an error for a non-array root. Invalid items remain in diagnostics.
pub fn load_processors(
    input: &Value,
    version: &Version,
) -> Result<ProcessorConfiguration, ConfigurationError> {
    let mut output = ProcessorConfiguration::default();
    for (index, definition) in array(input, "processors")?.iter().enumerate() {
        let identity = definition
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| format!("index:{index}"), String::from);
        let result = (|| {
            let definition = object(definition, "processor")?;
            let id = string(required(definition, "id")?, "id")?;
            if output.processors.contains_key(id) {
                return Err(ConfigurationError::DuplicateProcessor);
            }
            if !compatible(definition, version)? {
                return Ok(None);
            }
            processor(definition).map(Some)
        })();
        match result {
            Ok(Some(processor)) => {
                output.diagnostics.loaded.insert(identity.clone());
                output.processors.insert(identity, processor);
            }
            Ok(None) => {
                output.diagnostics.skipped.insert(identity);
            }
            Err(error) => output.diagnostics.failed(identity, &error),
        }
    }
    Ok(output)
}

/// Parse native processor scanner overrides.
///
/// # Errors
/// Returns an error for a non-array root. Invalid items remain in diagnostics.
pub fn load_processor_overrides(
    input: &Value,
) -> Result<OverrideConfiguration, ConfigurationError> {
    let mut output = OverrideConfiguration::default();
    for (index, definition) in array(input, "processor_overrides")?.iter().enumerate() {
        let identity = format!("index:{index}");
        let result = (|| {
            let definition = object(definition, "processor override")?;
            let targets = array(required(definition, "target")?, "target")?;
            if targets.is_empty() {
                return Err(ConfigurationError::InvalidDefinition(
                    "processor override without targets",
                ));
            }
            let targets = targets
                .iter()
                .map(reference)
                .collect::<Result<Vec<_>, _>>()?;
            if targets
                .iter()
                .any(|r| matches!(r, ScannerReference::Tags(_)))
            {
                return Err(ConfigurationError::InvalidDefinition(
                    "processor override with target by tags not supported",
                ));
            }
            let scanners = object(required(definition, "scanners")?, "scanners")?;
            Ok(ProcessorOverride {
                targets,
                scanners: ScannerSelection {
                    include: references(scanners, "include")?,
                    exclude: references(scanners, "exclude")?,
                },
            })
        })();
        match result {
            Ok(override_) => {
                output.overrides.push(override_);
                output.diagnostics.loaded.insert(identity);
            }
            Err(error) => output.diagnostics.failed(identity, &error),
        }
    }
    Ok(output)
}
