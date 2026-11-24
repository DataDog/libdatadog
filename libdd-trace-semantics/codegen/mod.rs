// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Build-time code generation: checks `mappings.json` and generates the registry's Rust code (the
//! `Concept` enum and its lookup tables), which `src/lib.rs` includes.
//!
//! Shared by `build.rs` and the `codegen` integration test, so the validation rules are tested
//! without going through a build. It only depends on `serde`, `serde_json`, `quote` and
//! `proc-macro2`. It is not under a `build/` directory because the workspace `.gitignore` ignores
//! those.

use core::fmt;
use std::collections::{HashMap, HashSet};

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};
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
    pub predicate: Predicate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Predicate {
    Present,
    Absent,
    Equals(String),
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
    let concepts = raw
        .concepts
        .0
        .into_iter()
        .map(|(name, concept)| {
            let concept = concept.validate(name)?;
            if let Some(other) = variants.insert(concept.variant.clone(), concept.name.clone()) {
                return Err(invalid(format!(
                    "concepts `{other}` and `{}` both map to the Rust name `{}`",
                    concept.name, concept.variant
                )));
            }
            Ok(concept)
        })
        .collect::<Result<_, _>>()?;

    Ok(Registry {
        version: raw.version,
        content_hash: raw.metadata.content_hash,
        concepts,
    })
}

impl RawConcept {
    fn validate(self, name: String) -> Result<Concept, Error> {
        if self.canonical != name {
            return Err(invalid(format!(
                "concept `{name}` has canonical name `{}`",
                self.canonical
            )));
        }
        if self.fallbacks.is_empty() {
            return Err(invalid(format!("concept `{name}` has no fallbacks")));
        }
        let variant = variant_name(&name)?;
        let entries = self
            .fallbacks
            .into_iter()
            .map(|entry| entry.validate(&name))
            .collect::<Result<_, _>>()?;
        Ok(Concept {
            name,
            variant,
            entries,
        })
    }
}

impl RawEntry {
    fn validate(self, concept: &str) -> Result<Entry, Error> {
        if self.name.is_empty() {
            return Err(invalid(format!(
                "concept `{concept}` has a fallback with no name"
            )));
        }
        if self.version.as_deref().is_some_and(str::is_empty) {
            return Err(invalid(format!(
                "fallback `{}` of `{concept}` has an empty version",
                self.name
            )));
        }
        let when = self
            .when
            .into_iter()
            .map(|c| c.validate(concept, &self.name))
            .collect::<Result<_, _>>()?;
        Ok(Entry {
            name: self.name,
            provider: self.provider,
            version: self.version,
            value_type: self.value_type.unwrap_or(ValueType::String),
            when,
        })
    }
}

impl RawCondition {
    fn validate(self, concept: &str, entry: &str) -> Result<Condition, Error> {
        let on = |what: &str| {
            invalid(format!(
                "fallback `{entry}` of `{concept}` has a condition on `{}` with {what}",
                self.attribute
            ))
        };
        if self.attribute.is_empty() {
            return Err(invalid(format!(
                "fallback `{entry}` of `{concept}` has a condition with no attribute"
            )));
        }
        // `eq` implies presence, and conditions treat an empty value as absent.
        let predicate = match (self.present, self.eq) {
            (None, None) => return Err(on("neither `eq` nor `present`")),
            (Some(false), Some(_)) => return Err(on("both `eq` and `present: false`")),
            (_, Some(eq)) if eq.is_empty() => return Err(on("an empty `eq`")),
            (_, Some(eq)) => Predicate::Equals(eq),
            (Some(true), None) => Predicate::Present,
            (Some(false), None) => Predicate::Absent,
        };
        Ok(Condition {
            attribute: self.attribute,
            predicate,
        })
    }
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

impl Registry {
    /// Writes the Rust source included by `src/lib.rs`.
    pub fn generate(&self) -> String {
        format!(
            "// @generated by build.rs from mappings.json. Do not edit.\n\n{}\n",
            self.tokens()
        )
    }

    fn tokens(&self) -> TokenStream {
        let variants: Vec<_> = self
            .concepts
            .iter()
            .map(|c| format_ident!("{}", c.variant))
            .collect();
        let names: Vec<_> = self.concepts.iter().map(|c| c.name.as_str()).collect();
        let docs = names.iter().map(|name| format!("`{name}`"));
        let entries = self.concepts.iter().map(|c| {
            let entries = c.entries.iter().map(Entry::tokens);
            quote! { &[#(#entries),*] }
        });
        let count = Literal::usize_unsuffixed(self.concepts.len());
        let version = &self.version;
        let content_hash = &self.content_hash;

        quote! {
            /// A semantic concept, such as the HTTP method, that one or more attributes can hold.
            ///
            /// Generated from `mappings.json`. New concepts may be added in any release.
            #[non_exhaustive]
            #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
            pub enum Concept {
                #(#[doc = #docs] #variants,)*
            }

            impl Concept {
                /// The canonical name of the concept, e.g. `http.method`.
                #[must_use]
                pub const fn name(self) -> &'static str {
                    match self {
                        #(Self::#variants => #names,)*
                    }
                }

                /// The attributes that can hold the concept, in precedence order.
                #[must_use]
                #[allow(clippy::match_same_arms)]
                pub const fn entries(self) -> &'static [Entry] {
                    match self {
                        #(Self::#variants => #entries,)*
                    }
                }
            }

            static CONCEPTS: [Concept; #count] = [#(Concept::#variants),*];

            fn concept_by_name(name: &str) -> Option<Concept> {
                match name {
                    #(#names => Some(Concept::#variants),)*
                    _ => None,
                }
            }

            const VERSION: &str = #version;
            const CONTENT_HASH: &str = #content_hash;
        }
    }
}

impl Entry {
    fn tokens(&self) -> TokenStream {
        let name = &self.name;
        let provider = match self.provider {
            Provider::Datadog => quote!(Provider::Datadog),
            Provider::Otel => quote!(Provider::Otel),
        };
        let version = self
            .version
            .as_ref()
            .map_or_else(|| quote!(None), |version| quote!(Some(#version)));
        let value_type = match self.value_type {
            ValueType::String => quote!(ValueType::String),
            ValueType::Int64 => quote!(ValueType::Int64),
            ValueType::Float64 => quote!(ValueType::Float64),
        };
        let when = self.when.iter().map(|c| {
            let attribute = &c.attribute;
            let predicate = match &c.predicate {
                Predicate::Present => quote!(Predicate::Present),
                Predicate::Absent => quote!(Predicate::Absent),
                Predicate::Equals(eq) => quote!(Predicate::Equals(#eq)),
            };
            quote! { Condition { attribute: #attribute, predicate: #predicate } }
        });
        quote! {
            Entry {
                name: #name,
                provider: #provider,
                version: #version,
                value_type: #value_type,
                when: &[#(#when),*],
            }
        }
    }
}
