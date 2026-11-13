// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Lookups over [`Attributes`].
//!
//! Each lookup walks the entries of a concept in precedence order, skips the ones whose
//! conditions don't hold, and returns the first value it can read with the entry's type. Empty
//! strings are skipped. In [`lookup_i64`] and [`lookup_f64`], strings are parsed as decimal
//! numbers, and strings that don't parse to a finite value fall through to the next entry. Numeric
//! attributes are returned as stored, except that [`lookup_i64`] skips values that are fractional,
//! not finite, or out of range, and [`lookup_str`] formats them with `Display`.
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

use crate::{Concept, Condition, Entry, ValueType};

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

fn str_in<'a, A: Attributes + ?Sized>(attrs: &'a A, entries: &[Entry]) -> Option<Cow<'a, str>> {
    matching(attrs, entries).find_map(|entry| match entry.value_type {
        ValueType::Int64 => attrs
            .attribute_i64(entry.name)
            .map(|v| Cow::Owned(v.to_string())),
        ValueType::Float64 => attrs
            .attribute_f64(entry.name)
            .map(|v| Cow::Owned(v.to_string())),
        ValueType::String => non_empty_str(attrs, entry.name).map(Cow::Borrowed),
    })
}

fn i64_in<A: Attributes + ?Sized>(attrs: &A, entries: &[Entry]) -> Option<i64> {
    matching(attrs, entries).find_map(|entry| match entry.value_type {
        // `attribute_i64` is `attribute_f64` + `f64_to_exact_i64`, except that it stays exact for
        // integers above 2^53.
        ValueType::Int64 | ValueType::Float64 => attrs.attribute_i64(entry.name),
        ValueType::String => {
            let v = non_empty_str(attrs, entry.name)?;
            v.parse::<i64>()
                .ok()
                .or_else(|| v.parse::<f64>().ok().and_then(f64_to_exact_i64))
        }
    })
}

fn f64_in<A: Attributes + ?Sized>(attrs: &A, entries: &[Entry]) -> Option<f64> {
    matching(attrs, entries).find_map(|entry| match entry.value_type {
        ValueType::Float64 => attrs.attribute_f64(entry.name),
        // Lossy above 2^53, like the Agent's `float64(v)`.
        #[allow(clippy::cast_precision_loss)]
        ValueType::Int64 => attrs.attribute_i64(entry.name).map(|v| v as f64),
        ValueType::String => non_empty_str(attrs, entry.name)?
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite()),
    })
}

fn matching<'e, A: Attributes + ?Sized>(
    attrs: &A,
    entries: &'e [Entry],
) -> impl Iterator<Item = &'e Entry> {
    entries
        .iter()
        .filter(move |entry| entry.when.iter().all(|c| condition_holds(attrs, c)))
}

fn non_empty_str<'a, A: Attributes + ?Sized>(attrs: &'a A, key: &str) -> Option<&'a str> {
    attrs.attribute_str(key).filter(|v| !v.is_empty())
}

/// Conditions read the exact attribute, as a string. An empty string counts as absent.
fn condition_holds<A: Attributes + ?Sized>(attrs: &A, condition: &Condition) -> bool {
    let value = non_empty_str(attrs, condition.attribute);
    if condition
        .present
        .is_some_and(|present| present != value.is_some())
    {
        return false;
    }
    condition.eq.is_none_or(|eq| value == Some(eq))
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

    const fn cond(
        attribute: &'static str,
        present: Option<bool>,
        eq: Option<&'static str>,
    ) -> Condition {
        Condition {
            attribute,
            present,
            eq,
        }
    }

    #[test]
    fn conditions() {
        // The embedded registry has no `present: true` condition, so test them on made-up entries.
        static ENTRIES: [Entry; 3] = [
            entry("a", &[cond("x", Some(true), None)]),
            entry(
                "b",
                &[cond("x", Some(false), None), cond("y", None, Some("on"))],
            ),
            entry("d", &[]),
        ];
        static PRESENT_AND_EQ: [Entry; 2] = [
            entry("c", &[cond("x", Some(true), Some("on"))]),
            entry("d", &[]),
        ];
        let attrs = |pairs: &[(&'static str, &'static str)]| {
            Attrs(
                [("a", "A"), ("b", "B"), ("c", "C"), ("d", "D")]
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

        assert_eq!(
            str_in(&attrs(&[("x", "on")]), &PRESENT_AND_EQ).as_deref(),
            Some("C")
        );
        assert_eq!(
            str_in(&attrs(&[("x", "off")]), &PRESENT_AND_EQ).as_deref(),
            Some("D")
        );
    }
}
