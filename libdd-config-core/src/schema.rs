// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use alloc::sync::Arc;
use std::{collections::HashMap, fs, path::Path};

use indexmap::IndexMap;
use libdd_regex::full::{Regex, RegexBuilder};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::{
    Error, ParsePolicy, Transform, TransformRegistry, Value, ValueKind, value::ParseOptions,
};

#[derive(Clone, Debug)]
pub struct ConfigSchema(pub(crate) Arc<Schema>);

#[derive(Debug)]
pub struct Schema {
    pub(crate) policy: ParsePolicy,
    pub(crate) rules: Vec<Rule>,
    pub(crate) names: HashMap<String, Name>,
    pub(crate) options: HashMap<String, usize>,
}

#[derive(Debug)]
pub struct Rule {
    pub(crate) name: String,
    pub(crate) property: String,
    pub(crate) kind: ValueKind,
    pub(crate) parse_options: ParseOptions,
    pub(crate) default: Option<Value>,
    pub(crate) validate_default: bool,
    pub(crate) allowed: Option<Arc<Regex>>,
    pub(crate) transform: Option<Transform>,
    pub(crate) sensitive: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Name {
    pub(crate) slot: usize,
    pub(crate) alias_rank: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    version: String,
    supported_configurations: IndexMap<String, Vec<Definition>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Definition {
    #[serde(rename = "type")]
    kind: ValueKind,
    default: Option<Box<RawValue>>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    configuration_names: Vec<String>,
    internal_property_name: Option<String>,
    namespace: Option<String>,
    transform: Option<String>,
    allowed: Option<Allowed>,
    #[serde(default)]
    sensitive: bool,
    #[serde(default)]
    alias_only: bool,
    #[serde(default)]
    parse: ParseOptions,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Allowed {
    Pattern(String),
    Options {
        pattern: String,
        #[serde(default, rename = "ignoreCase")]
        ignore_case: bool,
        #[serde(default, rename = "validateDefault")]
        validate_default: bool,
    },
}

impl ConfigSchema {
    /// Read a prepared tracer manifest and bind its configuration rules.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or its rules cannot be compiled.
    pub fn from_path(
        path: impl AsRef<Path>,
        policy: ParsePolicy,
        transforms: &TransformRegistry,
    ) -> Result<Self, Error> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|source| Error::Read {
            path: path.to_owned(),
            source,
        })?;
        Self::from_bytes(&bytes, policy, transforms)
    }

    /// Compile the same manifest from a packaged or virtual asset.
    ///
    /// # Errors
    /// Returns an error for an invalid schema, regexp, default, or transform binding.
    pub fn from_bytes(
        bytes: &[u8],
        policy: ParsePolicy,
        transforms: &TransformRegistry,
    ) -> Result<Self, Error> {
        let manifest: Manifest = serde_json::from_slice(bytes)?;
        if manifest.version != "2" {
            return Err(Error::UnsupportedManifest(manifest.version));
        }

        let mut rules = Vec::with_capacity(manifest.supported_configurations.len());
        let mut names = HashMap::with_capacity(manifest.supported_configurations.len());
        let mut aliases = Vec::new();
        let mut options = HashMap::new();
        let mut patterns = HashMap::new();
        for (name, definitions) in manifest.supported_configurations {
            let [definition] = <[Definition; 1]>::try_from(definitions)
                .map_err(|_| Error::AmbiguousDefinition(name.clone()))?;
            if definition.alias_only {
                continue;
            }

            let slot = rules.len();
            let property = definition.property(&name);
            if definition.allowed.is_some()
                && !matches!(
                    definition.kind,
                    ValueKind::Boolean
                        | ValueKind::Integer
                        | ValueKind::Decimal
                        | ValueKind::String
                )
            {
                return Err(Error::UnsupportedValidationKind(name));
            }
            let transform = definition
                .transform
                .as_deref()
                .map(|label| transforms.bind(&name, label, definition.kind))
                .transpose()?;
            let (allowed, validate_default) = definition
                .allowed
                .map(|allowed| allowed.compile(&name, &mut patterns))
                .transpose()?
                .unzip();
            let default = read_default(
                &name,
                definition.default.as_deref(),
                definition.kind,
                definition.parse,
                policy,
            )?;

            for option in definition.configuration_names {
                if options.insert(option.clone(), slot).is_some() {
                    return Err(Error::AmbiguousName(option));
                }
            }
            for (rank, alias) in definition.aliases.into_iter().enumerate() {
                aliases.push((
                    alias,
                    Name {
                        slot,
                        alias_rank: rank + 1,
                    },
                ));
            }
            names.insert(
                name.clone(),
                Name {
                    slot,
                    alias_rank: 0,
                },
            );
            rules.push(Rule {
                name,
                property,
                kind: definition.kind,
                parse_options: definition.parse,
                default,
                validate_default: validate_default.unwrap_or_default(),
                allowed,
                transform,
                sensitive: definition.sensitive,
            });
        }

        for (alias, name) in aliases {
            if let Some(&previous) = names.get(&alias) {
                if previous.alias_rank != 0 && previous.slot != name.slot {
                    return Err(Error::AmbiguousName(alias));
                }
            } else {
                names.insert(alias, name);
            }
        }

        Ok(Self(Arc::new(Schema {
            policy,
            rules,
            names,
            options,
        })))
    }
}

impl Definition {
    fn property(&self, name: &str) -> String {
        if let Some(namespace) = &self.namespace {
            format!("{namespace}.{name}")
        } else {
            self.internal_property_name
                .as_ref()
                .or_else(|| self.configuration_names.first())
                .map_or_else(|| name.to_owned(), Clone::clone)
        }
    }
}

impl Allowed {
    fn compile(
        self,
        name: &str,
        patterns: &mut HashMap<(String, bool), Arc<Regex>>,
    ) -> Result<(Arc<Regex>, bool), Error> {
        let (pattern, ignore_case, validate_default) = match self {
            Self::Pattern(pattern) => (pattern, false, true),
            Self::Options {
                pattern,
                ignore_case,
                validate_default,
            } => (pattern, ignore_case, validate_default),
        };

        let key = (pattern, ignore_case);
        if let Some(compiled) = patterns.get(&key) {
            return Ok((Arc::clone(compiled), validate_default));
        }

        let anchored = format!(r"\A(?:{})\z", key.0);
        let compiled = RegexBuilder::new(&anchored)
            .case_insensitive(ignore_case)
            .build()
            .map_err(|source| Error::Regex {
                key: name.to_owned(),
                source,
            })?;
        let compiled = Arc::new(compiled);
        patterns.insert(key, Arc::clone(&compiled));
        Ok((compiled, validate_default))
    }
}

fn read_default(
    name: &str,
    raw: Option<&RawValue>,
    kind: ValueKind,
    options: ParseOptions,
    policy: ParsePolicy,
) -> Result<Option<Value>, Error> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let raw = raw.get();
    let value = if raw.starts_with('"') {
        let raw: String = serde_json::from_str(raw)?;
        policy.parse(kind, options, &raw)
    } else if kind == ValueKind::Array && raw.starts_with('[') {
        serde_json::from_str::<Vec<String>>(raw)
            .ok()
            .map(|items| Value::Array(items.into_iter().map(Value::String).collect()))
    } else if kind == ValueKind::Map && raw.starts_with('{') {
        serde_json::from_str(raw).ok().map(Value::Map)
    } else {
        policy.parse(kind, options, raw)
    };
    value
        .map(Some)
        .ok_or_else(|| Error::InvalidDefault(name.to_owned()))
}
