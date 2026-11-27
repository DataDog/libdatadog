// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Lookups over [`Attributes`].
//!
//! Each lookup walks the entries of a concept in precedence order and returns the first value it
//! can read with the entry's type whose entry's conditions hold. The value is read first, so
//! conditions are only checked for entries that hold a value. Empty strings are skipped. In
//! [`lookup_i64`] and [`lookup_f64`], strings are parsed as decimal numbers, and strings that don't
//! parse to a finite value fall through to the next entry. Numeric attributes are returned as
//! stored, except that [`lookup_i64`] skips values that are fractional, not finite, or out of
//! range, and [`lookup_str`] formats them with `Display`. [`lookup_value`] returns the value with
//! the type it was read as, without allocating.
//!
//! Known differences with the Agent's Go lookups: hex floats and `_` digit separators are not
//! parsed, `"NaN"` and `"Inf"` strings fall through in [`lookup_f64`], and floats are formatted as
//! text with Rust's `Display` instead of Go's `%g`.
//!
//! Another difference comes from the attribute model rather than the registry:
//! [`Attributes::attribute_f64`] also returns integers, so a `float64` entry matches an integer
//! attribute on a V1 span, where the Agent's V1 accessor only matches doubles. On v0.4 spans,
//! numbers are all in `metrics` and both behave the same.

use alloc::borrow::Cow;
use alloc::string::ToString;

use libdd_trace_model::{Attributes, f64_to_exact_i64};

use crate::{Concept, Condition, Entry, Predicate, ValueType};

/// A value read by [`lookup_value`], with the type of the entry it was read from.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LookupValue<'a> {
    /// The value of a `string` entry, never empty
    String(&'a str),
    /// The value of an `int64` entry
    Int(i64),
    /// The value of a `float64` entry
    Float(f64),
}

/// The first value of `concept`, typed by its entry, `None` if no entry holds a value.
///
/// Like [`lookup_str`] without formatting numbers: string values are borrowed and empty strings
/// are skipped, `int64` entries read [`Attributes::attribute_i64`] and `float64` entries read
/// [`Attributes::attribute_f64`]. It never allocates.
///
/// Promoted tags can't be read with lookups: use the `Span` and `TraceChunk` getters instead.
#[must_use]
pub fn lookup_value<A: Attributes + ?Sized>(
    attrs: &A,
    concept: Concept,
) -> Option<LookupValue<'_>> {
    value_in(attrs, concept.entries())
}

/// The first value of `concept` as a string, `None` if no entry holds a value.
///
/// String values are borrowed, and empty strings are skipped. Numeric values of `int64` and
/// `float64` entries are formatted with `Display`, e.g. `1.0` as `"1"` and `1.5` as `"1.5"`.
///
/// Promoted tags can't be read with lookups: use the `Span` and `TraceChunk` getters instead.
#[must_use]
pub fn lookup_str<A: Attributes + ?Sized>(attrs: &A, concept: Concept) -> Option<Cow<'_, str>> {
    str_in(attrs, concept.entries())
}

/// The first value of `concept` as an integer, `None` if no entry holds one.
///
/// Strings are parsed as decimal integers, then as decimal floats. Floats that are fractional, not
/// finite, or out of range fall through to the next entry.
///
/// Promoted tags can't be read with lookups: use the `Span` and `TraceChunk` getters instead.
#[must_use]
pub fn lookup_i64<A: Attributes + ?Sized>(attrs: &A, concept: Concept) -> Option<i64> {
    i64_in(attrs, concept.entries())
}

/// The first value of `concept` as a float, `None` if no entry holds one.
///
/// Numeric attributes are returned as stored, integers converted to `f64`. Strings are parsed as
/// decimal floats, and strings that don't parse to a finite value (`"NaN"`, `"Inf"`, `"1e400"`)
/// fall through to the next entry.
///
/// Promoted tags can't be read with lookups: use the `Span` and `TraceChunk` getters instead.
#[must_use]
pub fn lookup_f64<A: Attributes + ?Sized>(attrs: &A, concept: Concept) -> Option<f64> {
    f64_in(attrs, concept.entries())
}

fn value_in<'a, A: Attributes + ?Sized>(
    attrs: &'a A,
    entries: &[Entry],
) -> Option<LookupValue<'a>> {
    entries.iter().find_map(|entry| {
        let value = match entry.value_type {
            ValueType::Int64 => attrs.attribute_i64(entry.name).map(LookupValue::Int),
            ValueType::Float64 => attrs.attribute_f64(entry.name).map(LookupValue::Float),
            ValueType::String => non_empty_str(attrs, entry.name).map(LookupValue::String),
        }?;
        conditions_hold(attrs, entry).then_some(value)
    })
}

fn str_in<'a, A: Attributes + ?Sized>(attrs: &'a A, entries: &[Entry]) -> Option<Cow<'a, str>> {
    Some(match value_in(attrs, entries)? {
        LookupValue::String(v) => Cow::Borrowed(v),
        LookupValue::Int(v) => Cow::Owned(v.to_string()),
        LookupValue::Float(v) => Cow::Owned(v.to_string()),
    })
}

fn i64_in<A: Attributes + ?Sized>(attrs: &A, entries: &[Entry]) -> Option<i64> {
    entries.iter().find_map(|entry| {
        let value = match entry.value_type {
            // `attribute_i64` is `attribute_f64` + `f64_to_exact_i64`, except that it stays exact
            // for integers above 2^53.
            ValueType::Int64 | ValueType::Float64 => attrs.attribute_i64(entry.name),
            ValueType::String => {
                let v = non_empty_str(attrs, entry.name)?;
                v.parse::<i64>()
                    .ok()
                    .or_else(|| v.parse::<f64>().ok().and_then(f64_to_exact_i64))
            }
        }?;
        conditions_hold(attrs, entry).then_some(value)
    })
}

fn f64_in<A: Attributes + ?Sized>(attrs: &A, entries: &[Entry]) -> Option<f64> {
    entries.iter().find_map(|entry| {
        let value = match entry.value_type {
            ValueType::Float64 => attrs.attribute_f64(entry.name),
            // Lossy above 2^53, like the Agent's `float64(v)`.
            #[allow(clippy::cast_precision_loss)]
            ValueType::Int64 => attrs.attribute_i64(entry.name).map(|v| v as f64),
            ValueType::String => non_empty_str(attrs, entry.name)?
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite()),
        }?;
        conditions_hold(attrs, entry).then_some(value)
    })
}

/// Whether all conditions of `entry` hold. Only checked for entries that hold a value: this avoids
/// condition reads when the value is absent, and replaces them with a value read when it is not.
fn conditions_hold<A: Attributes + ?Sized>(attrs: &A, entry: &Entry) -> bool {
    entry.when.iter().all(|c| condition_holds(attrs, c))
}

fn non_empty_str<'a, A: Attributes + ?Sized>(attrs: &'a A, key: &str) -> Option<&'a str> {
    attrs.attribute_str(key).filter(|v| !v.is_empty())
}

/// Conditions read the exact attribute, as a string. An empty string counts as absent.
fn condition_holds<A: Attributes + ?Sized>(attrs: &A, condition: &Condition) -> bool {
    let value = non_empty_str(attrs, condition.attribute);
    match condition.predicate {
        Predicate::Present => value.is_some(),
        Predicate::Absent => value.is_none(),
        Predicate::Equals(eq) => value == Some(eq),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Provider;
    use libdd_trace_model::StringData;
    use std::collections::HashMap;

    struct Attrs(HashMap<&'static str, &'static str>);

    impl Attributes for Attrs {
        type Text = String;
        type Bytes = Vec<u8>;
        type Values = StringData;

        fn attribute_str(&self, key: &str) -> Option<&str> {
            self.0.get(key).copied()
        }

        fn attribute_f64(&self, _: &str) -> Option<f64> {
            None
        }
    }

    const fn entry(name: &'static str, when: &'static [Condition]) -> Entry {
        Entry {
            name,
            provider: Provider::Otel,
            version: None,
            value_type: ValueType::String,
            when,
        }
    }

    const fn cond(attribute: &'static str, predicate: Predicate) -> Condition {
        Condition {
            attribute,
            predicate,
        }
    }

    #[test]
    fn conditions() {
        // The embedded registry has no `Present` condition, so test them on made-up entries.
        static ENTRIES: [Entry; 3] = [
            entry("a", &[cond("x", Predicate::Present)]),
            entry(
                "b",
                &[
                    cond("x", Predicate::Absent),
                    cond("y", Predicate::Equals("on")),
                ],
            ),
            entry("d", &[]),
        ];
        let attrs = |pairs: &[(&'static str, &'static str)]| {
            Attrs(
                [("a", "A"), ("b", "B"), ("d", "D")]
                    .into_iter()
                    .chain(pairs.iter().copied())
                    .collect(),
            )
        };
        let cases: &[(&[(&str, &str)], &str)] = &[
            (&[("x", "on")], "A"),
            (&[("x", "")], "D"),
            (&[("y", "on")], "B"),
            (&[("y", "off")], "D"),
            (&[], "D"),
        ];
        for (pairs, want) in cases {
            assert_eq!(
                str_in(&attrs(pairs), &ENTRIES).as_deref(),
                Some(*want),
                "{pairs:?}"
            );
        }
    }
}
