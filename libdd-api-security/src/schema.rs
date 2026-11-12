// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use alloc::borrow::Cow;
use alloc::vec::Vec;
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::classification::{Classification, Structural};
use crate::{
    MAX_ARRAY_NODES, MAX_CONTAINER_DEPTH, MAX_RECORD_NODES, Scanner, ScannerTags, SchemaOptions,
};

/// An extracted schema with property names borrowed from its input.
///
/// Serialize with `serde` to obtain the libddwaf schema wire format. Equality
/// compares structure, including array lengths and truncation metadata, rather
/// than the original values. The internal representation is not a public ABI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Schema<'a>(Node<'a>);

impl<'a> Schema<'a> {
    pub(crate) const fn from_node(node: Node<'a>) -> Self {
        Self(node)
    }

    pub(crate) fn from_json(
        input: &'a str,
        options: SchemaOptions,
    ) -> Result<Self, serde_json::Error> {
        crate::json::parse(input, options, Structural).map(Self)
    }

    pub(crate) fn extract(input: &'a Value) -> Self {
        Self(Node::extract(input, MAX_CONTAINER_DEPTH, Structural))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Node<'a> {
    Scalar(u8),
    TaggedString(&'a ScannerTags),
    Array {
        variants: Vec<Self>,
        len: usize,
        truncated: bool,
    },
    Record {
        fields: Vec<(Cow<'a, str>, Self)>,
        truncated: bool,
    },
}

impl<'a> Node<'a> {
    pub(crate) fn array(mut variants: Vec<Self>, len: usize, length_limit: Option<usize>) -> Self {
        // At most ten variants: canonical sorting also makes nested arrays
        // order-independent without a hash table per array.
        variants.sort_unstable();
        variants.dedup();
        Self::Array {
            variants,
            len: length_limit.map_or(len, |limit| len.min(limit)),
            truncated: len > MAX_ARRAY_NODES,
        }
    }

    pub(crate) fn record(mut fields: Vec<(Cow<'a, str>, Self)>, len: usize) -> Self {
        // Select before sorting. Stable sorting and key deduplication keep
        // the first entry for duplicate JSON keys, matching libddwaf.
        fields.sort_by(|left, right| left.0.cmp(&right.0));
        fields.dedup_by(|left, right| left.0 == right.0);
        Self::Record {
            fields,
            truncated: len > MAX_RECORD_NODES,
        }
    }

    pub(crate) fn string(value: &str, key: &str, scanners: &'a [Scanner]) -> Self {
        scanners
            .iter()
            .find(|scanner| scanner.matches(key, Some(value)))
            .map_or(Self::Scalar(8), |scanner| Self::classified(scanner.tags()))
    }

    pub(crate) fn classified(tags: &'a ScannerTags) -> Self {
        if tags.is_empty() {
            Self::Scalar(8)
        } else {
            Self::TaggedString(tags)
        }
    }

    pub(crate) fn extract(
        input: &'a Value,
        depth: usize,
        classify: impl Classification<'a>,
    ) -> Self {
        match input {
            Value::Null => Self::Scalar(1),
            Value::Bool(_) => Self::Scalar(2),
            Value::Number(number) => Self::Scalar(if number.is_i64() || number.is_u64() {
                4
            } else {
                16
            }),
            Value::String(value) => classify.string(value),
            Value::Array(input) => {
                let variants: Vec<_> = if depth > 1 {
                    input
                        .iter()
                        .take(MAX_ARRAY_NODES)
                        .map(|value| Self::extract(value, depth - 1, classify))
                        .collect()
                } else {
                    Vec::new()
                };
                Self::array(variants, input.len(), None)
            }
            Value::Object(input) => {
                let fields: Vec<_> = if depth > 1 {
                    input
                        .iter()
                        .take(MAX_RECORD_NODES)
                        .filter(|(key, _)| !key.is_empty())
                        .map(|(key, value)| {
                            (
                                Cow::Borrowed(key.as_str()),
                                Self::extract(value, depth - 1, classify.with_key(key)),
                            )
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                Self::record(fields, input.len())
            }
        }
    }
}

#[derive(Serialize)]
struct ArrayMetadata {
    len: usize,
    #[serde(skip_serializing_if = "core::ops::Not::not")]
    truncated: bool,
}

#[derive(Serialize)]
struct RecordMetadata {
    truncated: bool,
}

struct Fields<'a, 'b>(&'a [(Cow<'b, str>, Node<'b>)]);

impl Serialize for Fields<'_, '_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl Serialize for Node<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Scalar(code) => {
                let mut seq = serializer.serialize_seq(Some(1))?;
                seq.serialize_element(code)?;
                seq.end()
            }
            Self::TaggedString(tags) => {
                let mut seq = serializer.serialize_seq(Some(2))?;
                seq.serialize_element(&8_u8)?;
                seq.serialize_element(tags)?;
                seq.end()
            }
            Self::Array {
                variants,
                len,
                truncated,
            } => {
                let mut seq = serializer.serialize_seq(Some(2))?;
                seq.serialize_element(variants)?;
                seq.serialize_element(&ArrayMetadata {
                    len: *len,
                    truncated: *truncated,
                })?;
                seq.end()
            }
            Self::Record { fields, truncated } => {
                let mut seq = serializer.serialize_seq(Some(1 + usize::from(*truncated)))?;
                seq.serialize_element(&Fields(fields))?;
                if *truncated {
                    seq.serialize_element(&RecordMetadata { truncated: true })?;
                }
                seq.end()
            }
        }
    }
}
