// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Validates `mappings.json` and turns it into Rust source.
//!
//! Shared by `build.rs` and the `codegen` integration test, so the validation rules are tested
//! without going through a build. It only depends on `serde` and `serde_json`. It is not under a
//! `build/` directory because the workspace `.gitignore` ignores those.

use core::fmt::{self, Write as _};
use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};

/// Why `mappings.json` was rejected.
#[derive(Debug)]
pub enum Error {
    Json(serde_json::Error),
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "mappings.json is not valid: {e}"),
            Self::Invalid(msg) => write!(f, "mappings.json is not valid: {msg}"),
        }
    }
}

impl core::error::Error for Error {}

fn invalid(msg: impl Into<String>) -> Error {
    Error::Invalid(msg.into())
}

// Unknown fields are allowed at the top level and in `metadata`, which only carry labels.
// They are rejected below, where a new field could change what a lookup returns.
#[derive(Deserialize)]
struct RawRegistry {
    version: String,
    metadata: RawMetadata,
    concepts: Concepts,
}

#[derive(Deserialize)]
struct RawMetadata {
    content_hash: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConcept {
    canonical: String,
    fallbacks: Vec<RawEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEntry {
    name: String,
    provider: Provider,
    #[serde(default)]
    version: Option<String>,
    // The Agent treats a missing type like a string.
    #[serde(rename = "type", default)]
    value_type: Option<ValueType>,
    #[serde(default)]
    when: Vec<RawCondition>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCondition {
    attribute: String,
    #[serde(default)]
    present: Option<bool>,
    #[serde(default)]
    eq: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Datadog,
    Otel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueType {
    String,
    Int64,
    Float64,
}

/// The concepts in file order, rejecting duplicate keys (which `serde_json` would otherwise
/// resolve silently, last one wins).
struct Concepts(Vec<(String, RawConcept)>);

impl<'de> Deserialize<'de> for Concepts {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ConceptsVisitor;

        impl<'de> Visitor<'de> for ConceptsVisitor {
            type Value = Concepts;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of concepts")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Concepts, A::Error> {
                let mut seen = HashSet::new();
                let mut concepts = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, RawConcept>()? {
                    if !seen.insert(key.clone()) {
                        return Err(de::Error::custom(format!("duplicate concept `{key}`")));
                    }
                    concepts.push((key, value));
                }
                Ok(Concepts(concepts))
            }
        }

        deserializer.deserialize_map(ConceptsVisitor)
    }
}

/// A validated registry.
#[derive(Debug)]
pub struct Registry {
    pub version: String,
    pub content_hash: String,
    pub concepts: Vec<Concept>,
}

#[derive(Debug)]
pub struct Concept {
    /// The canonical name, e.g. `http.method`
    pub name: String,
    /// The Rust enum variant, e.g. `HttpMethod`
    pub variant: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug)]
pub struct Entry {
    pub name: String,
    pub provider: Provider,
    pub version: Option<String>,
    pub value_type: ValueType,
    pub when: Vec<Condition>,
}

#[derive(Debug)]
pub struct Condition {
    pub attribute: String,
    pub present: Option<bool>,
    pub eq: Option<String>,
}

/// Parses and validates `mappings.json`.
pub fn parse(json: &str) -> Result<Registry, Error> {
    let raw: RawRegistry = serde_json::from_str(json).map_err(Error::Json)?;

    if raw.version.trim().is_empty() {
        return Err(invalid("`version` is empty"));
    }
    if raw.metadata.content_hash.trim().is_empty() {
        return Err(invalid("`metadata.content_hash` is empty"));
    }
    if raw.concepts.0.is_empty() {
        return Err(invalid("there are no concepts"));
    }

    // Variant name -> concept name, to report collisions.
    let mut variants = HashMap::new();
    let mut concepts = Vec::with_capacity(raw.concepts.0.len());
    for (name, concept) in raw.concepts.0 {
        if concept.canonical != name {
            return Err(invalid(format!(
                "concept `{name}` has canonical name `{}`",
                concept.canonical
            )));
        }
        if concept.fallbacks.is_empty() {
            return Err(invalid(format!("concept `{name}` has no fallbacks")));
        }
        let variant = variant_name(&name)?;
        if let Some(other) = variants.insert(variant.clone(), name.clone()) {
            return Err(invalid(format!(
                "concepts `{other}` and `{name}` both map to the Rust name `{variant}`"
            )));
        }
        let entries = concept
            .fallbacks
            .into_iter()
            .map(|entry| validate_entry(&name, entry))
            .collect::<Result<_, _>>()?;
        concepts.push(Concept {
            name,
            variant,
            entries,
        });
    }

    Ok(Registry {
        version: raw.version,
        content_hash: raw.metadata.content_hash,
        concepts,
    })
}

fn validate_entry(concept: &str, entry: RawEntry) -> Result<Entry, Error> {
    if entry.name.is_empty() {
        return Err(invalid(format!(
            "concept `{concept}` has a fallback with no name"
        )));
    }
    if entry.version.as_deref().is_some_and(str::is_empty) {
        return Err(invalid(format!(
            "fallback `{}` of `{concept}` has an empty version",
            entry.name
        )));
    }
    let when = entry
        .when
        .into_iter()
        .map(|c| {
            if c.attribute.is_empty() {
                return Err(invalid(format!(
                    "fallback `{}` of `{concept}` has a condition with no attribute",
                    entry.name
                )));
            }
            // A condition without predicates always matches, which is never what was meant.
            if c.present.is_none() && c.eq.is_none() {
                return Err(invalid(format!(
                    "fallback `{}` of `{concept}` has a condition on `{}` with neither `eq` nor \
                     `present`",
                    entry.name, c.attribute
                )));
            }
            // Conditions treat an empty value as absent, so these can never match.
            if c.eq.as_deref() == Some("") {
                return Err(invalid(format!(
                    "fallback `{}` of `{concept}` has a condition on `{}` with an empty `eq`",
                    entry.name, c.attribute
                )));
            }
            if c.eq.is_some() && c.present == Some(false) {
                return Err(invalid(format!(
                    "fallback `{}` of `{concept}` has a condition on `{}` with both `eq` and \
                     `present: false`",
                    entry.name, c.attribute
                )));
            }
            Ok(Condition {
                attribute: c.attribute,
                present: c.present,
                eq: c.eq,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(Entry {
        name: entry.name,
        provider: entry.provider,
        version: entry.version,
        value_type: entry.value_type.unwrap_or(ValueType::String),
        when,
    })
}

/// The Rust variant name of a concept: the `PascalCase` of its `.`, `_` and `-` separated parts,
/// e.g. `peer.aws.s3.bucket` -> `PeerAwsS3Bucket` and `_dd.top_level` -> `DdTopLevel`.
pub fn variant_name(concept: &str) -> Result<String, Error> {
    if !concept
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(invalid(format!(
            "concept `{concept}` has characters other than ASCII letters, digits, `.`, `_` and `-`"
        )));
    }
    let mut variant = String::with_capacity(concept.len());
    for part in concept.split(['.', '_', '-']) {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            variant.push(first.to_ascii_uppercase());
            variant.extend(chars);
        }
    }
    // `Self` is the one keyword that is also a valid variant name.
    if !variant.starts_with(|c: char| c.is_ascii_alphabetic()) || variant == "Self" {
        return Err(invalid(format!(
            "concept `{concept}` does not give a valid Rust name"
        )));
    }
    Ok(variant)
}

/// Writes the Rust source included by `src/lib.rs`.
pub fn generate(registry: &Registry) -> String {
    let mut out = String::new();
    // Writing to a `String` cannot fail.
    let _ = write_registry(&mut out, registry);
    out
}

fn write_registry(out: &mut String, registry: &Registry) -> fmt::Result {
    writeln!(
        out,
        "// @generated by build.rs from mappings.json. Do not edit."
    )?;
    writeln!(out)?;

    writeln!(
        out,
        "/// A semantic concept, such as the HTTP method, that one or more attributes can hold."
    )?;
    writeln!(out, "///")?;
    writeln!(
        out,
        "/// Generated from `mappings.json`. New concepts may be added in any release."
    )?;
    writeln!(out, "#[non_exhaustive]")?;
    writeln!(
        out,
        "#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]"
    )?;
    writeln!(out, "pub enum Concept {{")?;
    for c in &registry.concepts {
        writeln!(out, "    #[doc = {:?}]", format!("`{}`", c.name))?;
        writeln!(out, "    {},", c.variant)?;
    }
    writeln!(out, "}}")?;
    writeln!(out)?;

    writeln!(out, "impl Concept {{")?;
    writeln!(
        out,
        "    /// The canonical name of the concept, e.g. `http.method`."
    )?;
    writeln!(out, "    #[must_use]")?;
    writeln!(
        out,
        "    #[allow(clippy::too_many_lines)] // one arm per concept"
    )?;
    writeln!(out, "    pub const fn name(self) -> &'static str {{")?;
    writeln!(out, "        match self {{")?;
    for c in &registry.concepts {
        writeln!(out, "            Self::{} => {:?},", c.variant, c.name)?;
    }
    writeln!(out, "        }}")?;
    writeln!(out, "    }}")?;
    writeln!(out)?;
    writeln!(
        out,
        "    /// The attributes that can hold the concept, in precedence order."
    )?;
    writeln!(out, "    #[must_use]")?;
    writeln!(
        out,
        "    #[allow(clippy::too_many_lines, clippy::match_same_arms)] // one arm per concept"
    )?;
    writeln!(out, "    pub const fn entries(self) -> &'static [Entry] {{")?;
    writeln!(out, "        match self {{")?;
    for c in &registry.concepts {
        writeln!(out, "            Self::{} => &[", c.variant)?;
        for e in &c.entries {
            write_entry(out, e)?;
        }
        writeln!(out, "            ],")?;
    }
    writeln!(out, "        }}")?;
    writeln!(out, "    }}")?;
    writeln!(out, "}}")?;
    writeln!(out)?;

    writeln!(
        out,
        "static CONCEPTS: [Concept; {}] = [",
        registry.concepts.len()
    )?;
    for c in &registry.concepts {
        writeln!(out, "    Concept::{},", c.variant)?;
    }
    writeln!(out, "];")?;
    writeln!(out)?;

    writeln!(
        out,
        "#[allow(clippy::too_many_lines)] // one arm per concept"
    )?;
    writeln!(out, "fn concept_by_name(name: &str) -> Option<Concept> {{")?;
    writeln!(out, "    match name {{")?;
    for c in &registry.concepts {
        writeln!(out, "        {:?} => Some(Concept::{}),", c.name, c.variant)?;
    }
    writeln!(out, "        _ => None,")?;
    writeln!(out, "    }}")?;
    writeln!(out, "}}")?;
    writeln!(out)?;

    writeln!(out, "const VERSION: &str = {:?};", registry.version)?;
    writeln!(
        out,
        "const CONTENT_HASH: &str = {:?};",
        registry.content_hash
    )?;
    Ok(())
}

fn write_entry(out: &mut String, e: &Entry) -> fmt::Result {
    let provider = match e.provider {
        Provider::Datadog => "Datadog",
        Provider::Otel => "Otel",
    };
    let value_type = match e.value_type {
        ValueType::String => "String",
        ValueType::Int64 => "Int64",
        ValueType::Float64 => "Float64",
    };
    writeln!(out, "                Entry {{")?;
    writeln!(out, "                    name: {:?},", e.name)?;
    writeln!(out, "                    provider: Provider::{provider},")?;
    writeln!(
        out,
        "                    version: {},",
        option_str(e.version.as_deref())
    )?;
    writeln!(
        out,
        "                    value_type: ValueType::{value_type},"
    )?;
    if e.when.is_empty() {
        writeln!(out, "                    when: &[],")?;
    } else {
        writeln!(out, "                    when: &[")?;
        for c in &e.when {
            let present = match c.present {
                Some(true) => "Some(true)",
                Some(false) => "Some(false)",
                None => "None",
            };
            writeln!(
                out,
                "                        Condition {{ attribute: {:?}, present: {present}, eq: {} }},",
                c.attribute,
                option_str(c.eq.as_deref())
            )?;
        }
        writeln!(out, "                    ],")?;
    }
    writeln!(out, "                }},")?;
    Ok(())
}

fn option_str(s: Option<&str>) -> String {
    s.map_or_else(|| "None".to_owned(), |s| format!("Some({s:?})"))
}
