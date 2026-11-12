// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::{Matcher, Scanner, ScannerError};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::{String, ToString},
};
use semver::Version;
use serde::Serialize;
use serde_json::{Map, Value};

/// A configuration error with a stable reason and typed underlying cause.
#[derive(Debug, thiserror::Error)]
pub enum ConfigurationError {
    /// A required field is absent.
    #[error("missing key '{0}'")]
    MissingKey(String),
    /// A field has the wrong JSON type.
    #[error("invalid type for '{field}': expected {expected}")]
    InvalidType {
        field: String,
        expected: &'static str,
    },
    /// The first successfully loaded scanner retains ownership of its ID.
    #[error("duplicate scanner")]
    DuplicateScanner,
    /// The first successfully loaded processor retains its ID.
    #[error("duplicate processor")]
    DuplicateProcessor,
    /// Only schema extraction is supported by this library.
    #[error("unknown generator: '{0}'")]
    UnknownGenerator(String),
    /// A collection configuration is structurally invalid.
    #[error("{0}")]
    InvalidDefinition(&'static str),
    /// Conditions must use a supported operator.
    #[error("unknown operator: '{0}'")]
    UnknownOperator(String),
    /// Scanner conditions cannot refer to mutable WAF rule data.
    #[error("dynamic data on scanner condition")]
    DynamicData,
    /// Minimum string length is nonnegative.
    #[error("min_length is a negative number")]
    NegativeLength,
    /// A version bound is not a semantic version.
    #[error("invalid version: {0}")]
    Version(#[source] semver::Error),
    /// A matcher could not be compiled.
    #[error(transparent)]
    Scanner(#[from] ScannerError),
}

/// Per-item diagnostics; invalid items do not discard valid neighbors.
#[derive(Debug, Default, Serialize)]
pub struct ConfigurationDiagnostics {
    /// Successfully compiled IDs.
    pub loaded: BTreeSet<String>,
    /// Failed IDs, or index:N when no string ID was available.
    pub failed: BTreeSet<String>,
    /// IDs outside the caller's compatibility version.
    pub skipped: BTreeSet<String>,
    /// Error messages mapped to affected item identities.
    pub errors: BTreeMap<String, BTreeSet<String>>,
    /// Unsupported operators are warnings, as in the reference parser.
    pub warnings: BTreeMap<String, BTreeSet<String>>,
}

impl ConfigurationDiagnostics {
    pub fn failed(&mut self, identity: String, error: &ConfigurationError) {
        self.failed.insert(identity.clone());
        let messages = if matches!(
            error,
            ConfigurationError::UnknownOperator(_) | ConfigurationError::UnknownGenerator(_)
        ) {
            &mut self.warnings
        } else {
            &mut self.errors
        };
        messages
            .entry(error.to_string())
            .or_default()
            .insert(identity);
    }
}

/// Compiled scanners and their load diagnostics.
#[derive(Debug, Default)]
pub struct ScannerConfiguration {
    /// Compiled scanners indexed by their configuration IDs.
    pub scanners: BTreeMap<String, Scanner>,
    /// Loading results for every supplied definition.
    pub diagnostics: ConfigurationDiagnostics,
}

pub fn invalid(field: &str, expected: &'static str) -> ConfigurationError {
    ConfigurationError::InvalidType {
        field: field.into(),
        expected,
    }
}

pub fn object<'a>(
    value: &'a Value,
    field: &str,
) -> Result<&'a Map<String, Value>, ConfigurationError> {
    value.as_object().ok_or_else(|| invalid(field, "map"))
}

pub fn required<'a>(
    value: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a Value, ConfigurationError> {
    value
        .get(field)
        .ok_or_else(|| ConfigurationError::MissingKey(field.into()))
}

pub fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str, ConfigurationError> {
    value.as_str().ok_or_else(|| invalid(field, "string"))
}

fn condition(value: &Value) -> Result<Matcher, ConfigurationError> {
    let condition = object(value, "condition")?;
    let operator = string(required(condition, "operator")?, "operator")?;
    let params = object(required(condition, "parameters")?, "parameters")?;
    match operator {
        "match_regex" => {
            let pattern = string(required(params, "regex")?, "regex")?;
            let empty = Map::new();
            let options = params
                .get("options")
                .map(|v| object(v, "options"))
                .transpose()?
                .unwrap_or(&empty);
            let min_length = options
                .get("min_length")
                .map(|v| {
                    let length = v.as_i64().ok_or_else(|| invalid("min_length", "integer"))?;
                    if length < 0 {
                        return Err(ConfigurationError::NegativeLength);
                    }
                    usize::try_from(length)
                        .map_err(|_| invalid("min_length", "target-sized nonnegative integer"))
                })
                .transpose()?
                .unwrap_or(0);
            let case_sensitive = options
                .get("case_sensitive")
                .map(|v| {
                    v.as_bool()
                        .ok_or_else(|| invalid("case_sensitive", "boolean"))
                })
                .transpose()?
                .unwrap_or(false);
            Matcher::regex(pattern, min_length, case_sensitive).map_err(Into::into)
        }
        "exact_match" | "ip_match" => {
            if params.contains_key("data") {
                return Err(ConfigurationError::DynamicData);
            }
            let list = required(params, "list")?
                .as_array()
                .ok_or_else(|| invalid("list", "array"))?;
            let values = list
                .iter()
                .map(|v| string(v, "list"))
                .collect::<Result<Vec<_>, _>>()?;
            if operator == "ip_match" {
                Matcher::ip(&values).map_err(Into::into)
            } else {
                Ok(Matcher::exact(
                    values.into_iter().map(String::from).collect(),
                ))
            }
        }
        _ => Err(ConfigurationError::UnknownOperator(operator.into())),
    }
}

pub fn compatible(
    value: &Map<String, Value>,
    version: &Version,
) -> Result<bool, ConfigurationError> {
    for (field, minimum) in [("min_version", true), ("max_version", false)] {
        if let Some(bound) = value.get(field) {
            let bound =
                Version::parse(string(bound, field)?).map_err(ConfigurationError::Version)?;
            if (minimum && version < &bound) || (!minimum && version > &bound) {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Compile native-format scanner definitions without constructing a WAF.
///
/// Version compatibility is explicit: the caller supplies the schema/rules
/// compatibility version it advertises. No environment or global state is read.
/// Definitions are processed in order; duplicate IDs retain the first success.
///
/// # Errors
/// Returns an error if the root is not an array. Per-scanner errors are returned
/// in diagnostics while valid definitions remain usable.
pub fn load_scanners(
    input: &Value,
    compatibility_version: &Version,
) -> Result<ScannerConfiguration, ConfigurationError> {
    let definitions = input
        .as_array()
        .ok_or_else(|| invalid("scanners", "array"))?;
    let mut output = ScannerConfiguration::default();
    for (index, definition) in definitions.iter().enumerate() {
        let identity = definition
            .get("id")
            .and_then(Value::as_str)
            .map_or_else(|| format!("index:{index}"), String::from);
        let result = (|| {
            let definition = object(definition, "scanner")?;
            let id = string(required(definition, "id")?, "id")?;
            if output.scanners.contains_key(id) {
                return Err(ConfigurationError::DuplicateScanner);
            }
            if !compatible(definition, compatibility_version)? {
                return Ok(None);
            }
            let tags = object(required(definition, "tags")?, "tags")?
                .iter()
                .map(|(key, value)| Ok((key.clone(), string(value, key)?.into())))
                .collect::<Result<_, ConfigurationError>>()?;
            let key = definition.get("key").map(condition).transpose()?;
            let value = definition.get("value").map(condition).transpose()?;
            Scanner::new(id.into(), tags, key, value)
                .map(Some)
                .map_err(Into::into)
        })();
        match result {
            Ok(Some(scanner)) => {
                output.diagnostics.loaded.insert(identity.clone());
                output.scanners.insert(identity, scanner);
            }
            Ok(None) => {
                output.diagnostics.skipped.insert(identity);
            }
            Err(error) => {
                output.diagnostics.failed(identity, &error);
            }
        }
    }
    Ok(output)
}
