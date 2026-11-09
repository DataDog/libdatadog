// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use alloc::borrow::Cow;
use alloc::vec::Vec;
use core::fmt;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::schema::Node;
use crate::{MAX_ARRAY_NODES, MAX_CONTAINER_DEPTH, MAX_RECORD_NODES, SchemaOptions};

pub fn parse(input: &str, options: SchemaOptions) -> Result<Node<'_>, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(input);
    let schema = Seed {
        depth: MAX_CONTAINER_DEPTH,
        options,
    }
    .deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(schema)
}

// Borrow unescaped keys; serde owns only keys that require unescaping.
#[derive(Deserialize)]
struct Key<'a>(#[serde(borrow)] Cow<'a, str>);

struct Seed {
    depth: usize,
    options: SchemaOptions,
}

impl<'de> DeserializeSeed<'de> for Seed {
    type Value = Node<'de>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Seed {
    type Value = Node<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(Node::Scalar(1))
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(Node::Scalar(2))
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(Node::Scalar(if self.options.numbers_as_float {
            16
        } else {
            4
        }))
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(Node::Scalar(if self.options.numbers_as_float {
            16
        } else {
            4
        }))
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(Node::Scalar(16))
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(Node::Scalar(8))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
        let mut variants = Vec::new();
        let mut len = 0;
        let limit = if self.depth > 1 { MAX_ARRAY_NODES } else { 0 };
        while len < limit {
            let Some(value) = input.next_element_seed(Self {
                depth: self.depth - 1,
                options: self.options,
            })?
            else {
                return Ok(self.array(variants, len));
            };
            variants.push(value);
            len += 1;
        }
        // Validate the discarded tail and count its original length without
        // allocating a DOM or descending through the schema visitor.
        while input.next_element::<IgnoredAny>()?.is_some() {
            len += 1;
        }
        Ok(self.array(variants, len))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
        let mut fields = Vec::new();
        let mut len = 0;
        let limit = if self.depth > 1 { MAX_RECORD_NODES } else { 0 };
        while len < limit {
            let Some(Key(key)) = input.next_key()? else {
                return Ok(Node::record(fields, len));
            };
            if key.is_empty() {
                input.next_value::<IgnoredAny>()?;
            } else {
                let value = input.next_value_seed(Self {
                    depth: self.depth - 1,
                    options: self.options,
                })?;
                fields.push((key, value));
            }
            len += 1;
        }
        while input.next_key::<IgnoredAny>()?.is_some() {
            input.next_value::<IgnoredAny>()?;
            len += 1;
        }
        Ok(Node::record(fields, len))
    }
}

impl Seed {
    fn array<'a>(&self, variants: Vec<Node<'a>>, original_len: usize) -> Node<'a> {
        let mut node = Node::array(variants, original_len);
        if let (Node::Array { len, .. }, Some(limit)) = (&mut node, self.options.array_length_limit)
        {
            *len = (*len).min(limit);
        }
        node
    }
}
