// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Structural API Security schemas, independent of `AppSec` and tracer bindings.
//!
//! This proof of concept extracts types and property names from JSON, without
//! PII classification. Sampling, compression, transport and configuration are
//! the caller's responsibility.
//!
//! [`extract_schema`] borrows an already parsed JSON value; [`extract_schema_json`]
//! parses directly into a schema and serializes the result. It still scans the
//! entire JSON input for validity and lengths, so cost grows with input size.
//! There are no total input-byte, node, output-byte or execution-time budgets.
//!
//! Schemas follow libddwaf's scanner-free encoding, with deterministic ordering.
//! JSON cannot represent libddwaf's invalid/unknown values. The JSON entry point
//! selects fields in source order and keeps the first selected duplicate key.
//! The parsed-value entry point uses the supplied map's iteration order; it
//! cannot recover source order or duplicate entries already lost during parsing.

extern crate alloc;

mod json;
mod schema;

use alloc::string::String;
use serde_json::Value;

pub use schema::Schema;

/// Maximum number of visited value levels, including the root.
///
/// A container at level 18 has no extracted children. Depth exhaustion alone
/// does not add a `truncated` marker, matching libddwaf.
pub const MAX_CONTAINER_DEPTH: usize = 18;
/// Maximum number of array elements visited before structural deduplication.
pub const MAX_ARRAY_NODES: usize = 10;
/// Maximum number of record entries examined, including skipped empty keys.
pub const MAX_RECORD_NODES: usize = 255;

/// An error at the JSON input or output boundary.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    /// The input is not a JSON value accepted by `serde_json`.
    #[error("invalid JSON input: {0}")]
    InvalidJson(#[source] serde_json::Error),
    /// The extracted schema could not be serialized.
    #[error("could not serialize schema: {0}")]
    Serialization(#[source] serde_json::Error),
}

/// Extract a schema from an already parsed value.
///
/// Property names borrow from `input`; scalar values are not retained in the
/// schema. Array lengths describe the supplied value, which a caller may have
/// truncated before calling this function. Empty property names are skipped.
///
/// Map selection follows the supplied map's iteration order (key-sorted with
/// `serde_json`'s default map). Use [`extract_schema_json`] for JSON source order.
/// Record fields and array variants are canonicalized after selecting children.
/// Thus structural equality and serialization are independent of field/variant
/// order within the selected portion of the input.
///
/// # Examples
///
/// ```
/// use libdd_api_security::extract_schema;
/// use serde_json::json;
///
/// let input = json!({"name": "Ada", "active": true, "id": 42});
/// let schema = extract_schema(&input);
/// assert_eq!(
///     serde_json::to_value(&schema)?,
///     json!([{"active": [2], "id": [4], "name": [8]}]),
/// );
/// # Ok::<(), serde_json::Error>(())
/// ```
#[must_use]
pub fn extract_schema(input: &Value) -> Schema<'_> {
    Schema::extract(input)
}

/// Parse one JSON value and return its schema as compact JSON.
///
/// No scalar values are copied into the output. Record keys are retained, so
/// this is not a general-purpose redactor for data stored in property names.
///
/// # Errors
///
/// Returns [`SchemaError::InvalidJson`] for malformed input, trailing values,
/// or a visited numeric value outside the parser's supported range. Discarded
/// subtrees are syntax-checked using `serde::de::IgnoredAny`. Returns
/// [`SchemaError::Serialization`] if output serialization fails.
///
/// # Examples
///
/// ```
/// use libdd_api_security::extract_schema_json;
///
/// assert_eq!(
///     extract_schema_json(r#"["a", "b", "c"]"#)?,
///     r#"[[[8]],{"len":3}]"#
/// );
/// # Ok::<(), libdd_api_security::SchemaError>(())
/// ```
pub fn extract_schema_json(input: &str) -> Result<String, SchemaError> {
    let schema = Schema::from_json(input).map_err(SchemaError::InvalidJson)?;
    serde_json::to_string(&schema).map_err(SchemaError::Serialization)
}
