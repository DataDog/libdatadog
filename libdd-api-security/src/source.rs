// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use alloc::borrow::Cow;
use alloc::vec::Vec;

use crate::schema::Node;
use crate::{MAX_ARRAY_NODES, MAX_CONTAINER_DEPTH, MAX_RECORD_NODES, Schema, SchemaOptions};

/// A scalar's structural type. Its contents are never requested.
#[derive(Clone, Copy, Debug)]
pub enum ScalarType {
    /// JSON null.
    Null,
    /// Boolean.
    Boolean,
    /// Integer.
    Integer,
    /// Floating-point number.
    Float,
    /// String.
    String,
}

/// A value's type and container size, without reading its children.
#[derive(Clone, Copy, Debug)]
pub enum SourceKind {
    /// A scalar value.
    Scalar(ScalarType),
    /// An array with its original element count.
    Array(usize),
    /// An object with its original entry count.
    Record(usize),
}

/// Lazy access to a caller-owned value tree.
///
/// Only selected children are requested: at most ten array elements or 255
/// record entries per visited container, and at most 18 value levels. An
/// adapter may omit unsupported record values by returning \`None\`; these
/// entries still consume the selection quota. Empty keys also consume quota.
///
/// Adapters own language-specific conversions, cycle detection and errors.
/// Implementations must keep container sizes and entry order stable during
/// extraction. This interface provides traversal limits, not a total work or
/// time budget: enumerating keys or running an application getter may be costly.
pub trait SchemaSource<'a>: Sized {
    /// An error reported by the input adapter.
    type Error;

    /// Inspect this value without reading children or scalar contents.
    fn kind(&self) -> SourceKind;

    /// Read a selected array element. Called only on arrays, within their size.
    ///
    /// # Errors
    /// Returns the adapter's error if reading the selected element fails.
    fn element(&self, index: usize) -> Result<Self, Self::Error>;

    /// Read a selected record entry. Called only on records, within their size.
    ///
    /// An empty key should be returned without reading its value.
    ///
    /// # Errors
    /// Returns the adapter's error if reading the selected entry fails.
    fn entry(&self, index: usize) -> Result<(Cow<'a, str>, Option<Self>), Self::Error>;
}

/// Extract a schema by reading only selected children of a foreign value tree.
///
/// Unlike the JSON text API, this does not validate ignored subtrees. The
/// adapter reports original container lengths; representation options apply
/// before structural deduplication.
///
/// # Errors
///
/// Propagates an adapter error from a visited value. Ignored children cannot
/// produce errors because they are never requested.
pub fn extract_schema_from_source<'a, S: SchemaSource<'a>>(
    input: &S,
    options: SchemaOptions,
) -> Result<Schema<'a>, S::Error> {
    extract(input, MAX_CONTAINER_DEPTH, options).map(Schema::from_node)
}

fn extract<'a, S: SchemaSource<'a>>(
    input: &S,
    depth: usize,
    options: SchemaOptions,
) -> Result<Node<'a>, S::Error> {
    Ok(match input.kind() {
        SourceKind::Scalar(kind) => Node::Scalar(match kind {
            ScalarType::Null => 1,
            ScalarType::Boolean => 2,
            ScalarType::Integer if !options.numbers_as_float => 4,
            ScalarType::Integer | ScalarType::Float => 16,
            ScalarType::String => 8,
        }),
        SourceKind::Array(len) => {
            let mut values = Vec::new();
            if depth > 1 {
                values.reserve(len.min(MAX_ARRAY_NODES));
                for index in 0..len.min(MAX_ARRAY_NODES) {
                    values.push(extract(&input.element(index)?, depth - 1, options)?);
                }
            }
            let mut node = Node::array(values, len);
            if let Node::Array { len, .. } = &mut node
                && let Some(limit) = options.array_length_limit
            {
                *len = (*len).min(limit);
            }
            node
        }
        SourceKind::Record(len) => {
            let mut fields = Vec::new();
            if depth > 1 {
                fields.reserve(len.min(MAX_RECORD_NODES));
                for index in 0..len.min(MAX_RECORD_NODES) {
                    let (key, value) = input.entry(index)?;
                    if !key.is_empty()
                        && let Some(value) = value
                    {
                        fields.push((key, extract(&value, depth - 1, options)?));
                    }
                }
            }
            Node::record(fields, len)
        }
    })
}
