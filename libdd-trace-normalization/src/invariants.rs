// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The guarantees of the normalization functions, checked by the property tests and the fuzz
//! targets.
//!
//! Each `check_*` function runs a normalization function on its input, asserts the guarantees of
//! the result, and returns it. The tag and name normalizers edit the bytes of a `String` in
//! place, so their result is also compared with a simple implementation of the same rules that
//! builds a new `String` (see [`reference_normalize`] and [`reference_metric_name`]).

use crate::normalize_utils::{
    self, DEFAULT_SERVICE_NAME, DEFAULT_SPAN_NAME, MAX_NAME_LEN, MAX_SERVICE_LEN, MAX_TAG_LEN,
    MAX_TYPE_LEN, YEAR_2000_NANOSEC_TS,
};
use crate::normalizer::{self, is_valid_http_status_code};
use crate::normalizer_v1;
use crate::test_model::{TestLink, TestSpan, Value};
use libdd_trace_model::AttributeRef;
use libdd_trace_protobuf::pb;
use std::collections::HashMap;

const HTTP_STATUS_CODE_KEY: &str = "http.status_code";
const LINK_NAME_KEY: &str = "link.name";

/// Asserts that `s` holds valid UTF-8. The normalizers write the bytes of a `String` directly, so
/// a bug there could leave invalid UTF-8 in it without failing.
fn assert_utf8(s: &str) {
    assert!(
        std::str::from_utf8(s.as_bytes()).is_ok(),
        "invalid UTF-8: {:?}",
        s.as_bytes()
    );
}

/// Lowercases `c` if its lowercase character has the same UTF-8 width.
fn lowercase_same_width(c: char) -> char {
    match c.to_lowercase().next() {
        Some(lower) if c.is_uppercase() && lower.len_utf8() == c.len_utf8() => lower,
        _ => c,
    }
}

/// The rules of `normalize_utils::normalize`, building a new `String` instead of editing the
/// input in place, and without its ASCII fast path.
fn reference_normalize(tag: &str, remove_digit_start_char: bool) -> String {
    let mut out = String::new();
    let mut read = 0;
    let mut chars = 0;
    let mut pending_underscore = false;
    for c in tag.chars() {
        read += c.len_utf8();
        let c = lowercase_same_width(c);
        let keep = if c.is_alphabetic() || c == ':' {
            true
        } else if remove_digit_start_char && chars == 0 {
            false
        } else if c.is_numeric() || matches!(c, '.' | '/' | '-') {
            true
        } else {
            chars += 1;
            pending_underscore = true;
            false
        };
        if keep {
            chars += 1;
            if std::mem::take(&mut pending_underscore) {
                out.push('_');
            }
            out.push(c);
        }
        if read >= 2 * MAX_TAG_LEN || chars >= MAX_TAG_LEN {
            break;
        }
    }
    out
}

/// The rules of `normalize_utils::normalize_metric_name`, building a new `String` instead of
/// editing the input in place, and working on characters instead of bytes.
fn reference_metric_name(name: &str) -> String {
    let Some(start) = name.find(|c: char| c.is_ascii_alphabetic()) else {
        return String::new();
    };
    let name = &name[start..];
    let mut out = String::new();
    if is_valid_metric_name(name) {
        out.push_str(name);
    } else {
        for c in name.chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c);
            } else if c == '.' {
                // An `_` before a `.` is replaced by the `.`.
                if out.ends_with('_') {
                    out.pop();
                }
                out.push('.');
            } else if !out.ends_with(['.', '_']) {
                out.push('_');
            }
        }
    }
    if out.ends_with('_') {
        out.pop();
    }
    out
}

/// Whether `name` is a valid normalized metric name: an ASCII letter, then ASCII letters, digits,
/// `.` and `_`, with no `__` and no `_.`.
fn is_valid_metric_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_'))
        && !name.contains("__")
        && !name.contains("_.")
}

/// Checks the result of a tag normalizer: `normalize_tag` if `is_tag`, else `normalize_tag_value`.
fn check_tag_like(input: &str, out: &str, is_tag: bool) {
    assert_utf8(out);
    assert_eq!(out, reference_normalize(input, is_tag), "input: {input:?}");
    assert!(
        out.chars().count() <= MAX_TAG_LEN,
        "more than {MAX_TAG_LEN} characters: {out:?}"
    );
    // The last character read can start just before the byte limit, and is at most 4 bytes.
    assert!(
        out.len() <= input.len().min(2 * MAX_TAG_LEN + 3),
        "{} bytes for an input of {}: {out:?}",
        out.len(),
        input.len()
    );
    assert!(
        out.chars().all(|c| c.is_alphabetic()
            || c.is_numeric()
            || matches!(c, ':' | '.' | '/' | '-' | '_')),
        "illegal character in {out:?}"
    );
    assert!(
        !out.chars().any(|c| c.is_ascii_uppercase()),
        "uppercase ASCII in {out:?}"
    );
    assert!(
        !out.contains("__") && !out.ends_with('_'),
        "`__` or trailing `_` in {out:?}"
    );
    if is_tag {
        assert!(
            out.chars()
                .next()
                .is_none_or(|c| c.is_alphabetic() || c == ':'),
            "a tag starts with a letter or `:`: {out:?}"
        );
    }
    let mut again = out.to_owned();
    if is_tag {
        normalize_utils::normalize_tag(&mut again);
    } else {
        normalize_utils::normalize_tag_value(&mut again);
    }
    assert_eq!(again, out, "normalizing again changes the result");
}

/// Checks [`normalize_utils::normalize_tag`] on `input` and returns its result.
pub(crate) fn check_normalize_tag(input: &str) -> String {
    let mut out = input.to_owned();
    normalize_utils::normalize_tag(&mut out);
    check_tag_like(input, &out, true);
    out
}

/// Checks [`normalize_utils::normalize_tag_value`] on `input` and returns its result.
pub(crate) fn check_normalize_tag_value(input: &str) -> String {
    let mut out = input.to_owned();
    normalize_utils::normalize_tag_value(&mut out);
    check_tag_like(input, &out, false);
    out
}

/// Checks [`normalize_utils::truncate_utf8`] on `input` and returns its result.
pub(crate) fn check_truncate_utf8(input: &str, limit: usize) -> String {
    let mut out = input.to_owned();
    normalize_utils::truncate_utf8(&mut out, limit);
    assert_utf8(&out);
    assert!(input.starts_with(out.as_str()), "{out:?} isn't a prefix");
    if input.len() <= limit {
        assert_eq!(out, input, "truncated below the limit");
    } else {
        // Only the character that crosses the limit is removed.
        assert!(out.len() <= limit, "{} bytes, limit {limit}", out.len());
        assert!(
            input[out.len()..]
                .chars()
                .next()
                .is_some_and(|c| out.len() + c.len_utf8() > limit),
            "truncated more than needed to {} bytes, limit {limit}",
            out.len()
        );
    }
    out
}

/// Checks [`normalize_utils::normalize_name`] on `input` and returns its result.
pub(crate) fn check_normalize_name(input: &str) -> String {
    let mut out = input.to_owned();
    normalize_utils::normalize_name(&mut out);
    assert_utf8(&out);
    let expected = reference_metric_name(&check_truncate_utf8(input, MAX_NAME_LEN));
    if expected.is_empty() {
        assert_eq!(out, DEFAULT_SPAN_NAME, "input: {input:?}");
    } else {
        assert_eq!(out, expected, "input: {input:?}");
    }
    assert!(out.len() <= MAX_NAME_LEN, "{} bytes: {out:?}", out.len());
    assert!(
        is_valid_metric_name(&out) && !out.ends_with('_'),
        "invalid name {out:?}"
    );
    let mut again = out.clone();
    normalize_utils::normalize_name(&mut again);
    assert_eq!(again, out, "normalizing again changes the result");
    out
}

/// Checks [`normalize_utils::normalize_peer_service`] on `input` and returns its result.
pub(crate) fn check_normalize_peer_service(input: &str) -> String {
    let mut out = input.to_owned();
    normalize_utils::normalize_peer_service(&mut out);
    check_tag_like(&check_truncate_utf8(input, MAX_SERVICE_LEN), &out, false);
    assert!(out.len() <= MAX_SERVICE_LEN, "{} bytes: {out:?}", out.len());
    out
}

/// Checks [`normalize_utils::normalize_service_with_lang`] on `input` and returns its result.
pub(crate) fn check_normalize_service(input: &str, lang: &str) -> String {
    let mut out = input.to_owned();
    normalize_utils::normalize_service_with_lang(&mut out, lang);
    assert_utf8(&out);
    let normalized = check_normalize_peer_service(input);
    if !normalized.is_empty() {
        assert_eq!(out, normalized);
    } else if lang.is_empty() {
        assert_eq!(out, DEFAULT_SERVICE_NAME);
    } else {
        assert_eq!(out, format!("unnamed-{lang}-service"));
    }
    out
}

/// Checks [`normalize_utils::normalize_span_type`] on `input` and returns its result.
pub(crate) fn check_normalize_span_type(input: &str) -> String {
    let mut out = input.to_owned();
    normalize_utils::normalize_span_type(&mut out);
    assert_eq!(out, check_truncate_utf8(input, MAX_TYPE_LEN));
    out
}

/// Checks the start and duration `after` normalizing those `before`, see
/// [`normalize_utils::normalize_span_start_duration`]. The start depends on the current time, so
/// it is only checked to be in range when it was replaced.
pub(crate) fn check_start_duration(before: (i64, i64), after: (i64, i64)) {
    let (start, duration) = before;
    let expected_duration = if duration < 0 || start.checked_add(duration).is_none() {
        0
    } else {
        duration
    };
    assert_eq!(after.1, expected_duration, "duration of {before:?}");
    if start >= YEAR_2000_NANOSEC_TS {
        assert_eq!(after.0, start, "start of {before:?}");
    } else {
        assert!(after.0 >= 0, "start of {before:?} became {}", after.0);
    }
}

/// Checks [`normalize_utils::normalize_span_start_duration`] on `start` and `duration`.
#[cfg(test)]
pub(crate) fn check_normalize_span_start_duration(start: i64, duration: i64) {
    let mut after = (start, duration);
    normalize_utils::normalize_span_start_duration(&mut after.0, &mut after.1);
    check_start_duration((start, duration), after);
}

/// Checks the fields of a v0.4 span that [`normalizer::normalize_span`] may change.
fn check_normalized_pb_fields(before: &pb::Span, after: &pb::Span) {
    assert_eq!(after.service, check_normalize_service(&before.service, ""));
    assert_eq!(after.name, check_normalize_name(&before.name));
    if before.resource.is_empty() {
        assert_eq!(after.resource, after.name);
    } else {
        assert_eq!(after.resource, before.resource);
    }
    let parent_is_root = before.parent_id == before.trace_id && before.parent_id == before.span_id;
    assert_eq!(
        after.parent_id,
        if parent_is_root { 0 } else { before.parent_id }
    );
    check_start_duration(
        (before.start, before.duration),
        (after.start, after.duration),
    );
    assert_eq!(after.r#type, check_normalize_span_type(&before.r#type));

    let mut meta = before.meta.clone();
    if let Some(env) = meta.get_mut("env") {
        *env = check_normalize_tag_value(env);
    }
    meta.retain(|k, v| k != HTTP_STATUS_CODE_KEY || is_valid_http_status_code(v));
    assert_eq!(after.meta, meta);
}

/// Checks [`normalizer::normalize_span`] on `span`.
pub(crate) fn check_normalize_span(span: &pb::Span) {
    let mut out = span.clone();
    let result = normalizer::normalize_span(&mut out);
    if span.trace_id == 0 || span.span_id == 0 {
        assert!(result.is_err(), "a zero ID is rejected");
        return;
    }
    assert!(result.is_ok(), "{result:?}");
    check_normalized_pb_fields(span, &out);
    // The other fields are unchanged. They are compared through `Debug`, as a metric can be NaN.
    let rest = |s: &pb::Span| {
        format!(
            "{:?}",
            (
                s.trace_id,
                s.span_id,
                s.error,
                &s.metrics,
                &s.meta_struct,
                &s.span_links,
                &s.span_events
            )
        )
    };
    assert_eq!(rest(&out), rest(span));

    // Normalizing again changes nothing, but a start before the year 2000 may be replaced again.
    let mut again = out.clone();
    assert!(normalizer::normalize_span(&mut again).is_ok());
    (again.start, again.duration) = (out.start, out.duration);
    assert_eq!(
        rest(&again),
        rest(&out),
        "normalizing again changes the span"
    );
    assert_eq!(
        (&again.service, &again.name, &again.resource, &again.r#type),
        (&out.service, &out.name, &out.resource, &out.r#type),
        "normalizing again changes the span"
    );
    assert_eq!(
        (again.parent_id, &again.meta),
        (out.parent_id, &out.meta),
        "normalizing again changes the span"
    );
}

/// Whether `a` and `b` are equal, NaN floats included.
fn value_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| value_eq(a, b))
        }
        (Value::KeyValueList(a), Value::KeyValueList(b)) => attrs_eq(a, b),
        _ => a == b,
    }
}

/// Whether `a` and `b` are equal, NaN floats included.
fn attrs_eq(a: &HashMap<String, Value>, b: &HashMap<String, Value>) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|(k, v)| b.get(k).is_some_and(|w| value_eq(v, w)))
}

/// Checks the `link.name` of `link` after normalization.
fn check_normalized_link(before: &TestLink, after: &TestLink) {
    let mut expected = before.attrs.clone();
    if let Some(value) = before.attrs.get(LINK_NAME_KEY) {
        let name = check_normalize_name(
            &AttributeRef::from(value).format_as_string_truncated(MAX_NAME_LEN),
        );
        // Formatting only the first `MAX_NAME_LEN` bytes gives the same name.
        assert_eq!(name, check_normalize_name(&value.format_as_string()));
        if value.as_str() != Some(name.as_str()) {
            expected.insert(LINK_NAME_KEY.to_owned(), Value::String(name));
        }
    }
    assert!(
        attrs_eq(&after.attrs, &expected),
        "link attributes {:?}, expected {expected:?}",
        after.attrs
    );
}

/// Checks the span `after` normalizing `before` with [`normalizer_v1::normalize_span`], except
/// its start and duration.
fn check_normalized_v1_fields(before: &TestSpan, after: &TestSpan, lang: &str) {
    assert_eq!(
        after.service,
        check_normalize_service(&before.service, lang)
    );
    assert_eq!(after.name, check_normalize_name(&before.name));
    if before.resource.is_empty() {
        assert_eq!(after.resource, after.name);
    } else {
        assert_eq!(after.resource, before.resource);
    }
    let parent_id = if before.parent_id == before.span_id {
        0
    } else {
        before.parent_id
    };
    assert_eq!(after.parent_id, parent_id);
    assert_eq!(after.typ, check_normalize_span_type(&before.typ));
    let env = match before.env.as_deref() {
        Some(env) if !env.is_empty() => Some(check_normalize_tag_value(env)),
        env => env.map(str::to_owned),
    };
    assert_eq!(after.env, env);

    let mut attrs = before.attrs.clone();
    for key in ["peer.service", "_dd.base_service"] {
        if let Some(Value::String(v)) = attrs.get_mut(key)
            && !v.is_empty()
        {
            *v = check_normalize_peer_service(v);
        }
    }
    attrs.retain(|k, v| {
        k != HTTP_STATUS_CODE_KEY
            || match v {
                Value::Array(_) | Value::KeyValueList(_) => false,
                v => is_valid_http_status_code(&v.format_as_string()),
            }
    });
    assert!(
        attrs_eq(&after.attrs, &attrs),
        "span attributes {:?}, expected {attrs:?}",
        after.attrs
    );

    assert_eq!(after.links.len(), before.links.len());
    for (before, after) in before.links.iter().zip(&after.links) {
        check_normalized_link(before, after);
    }
}

/// Checks [`normalizer_v1::normalize_span`] on `span` with `lang`.
pub(crate) fn check_normalize_span_v1(span: &TestSpan, lang: &str) {
    let mut out = span.clone();
    let result = normalizer_v1::normalize_span(&mut out, lang);
    if span.span_id == 0 {
        assert!(result.is_err(), "a zero span ID is rejected");
        return;
    }
    assert!(result.is_ok(), "{result:?}");
    check_normalized_v1_fields(span, &out, lang);
    check_start_duration((span.start, span.duration), (out.start, out.duration));

    // Normalizing again changes nothing, but a start before the year 2000 may be replaced again.
    let mut again = out.clone();
    assert!(normalizer_v1::normalize_span(&mut again, lang).is_ok());
    (again.start, again.duration) = (out.start, out.duration);
    assert!(
        v1_span_eq(&again, &out),
        "normalizing again changes the span: {again:?}, was {out:?}"
    );
}

/// Whether `a` and `b` are equal, NaN floats included.
fn v1_span_eq(a: &TestSpan, b: &TestSpan) -> bool {
    (&a.service, &a.name, &a.resource, &a.typ, &a.env)
        == (&b.service, &b.name, &b.resource, &b.typ, &b.env)
        && (a.span_id, a.parent_id, a.start, a.duration)
            == (b.span_id, b.parent_id, b.start, b.duration)
        && attrs_eq(&a.attrs, &b.attrs)
        && a.links.len() == b.links.len()
        && a.links
            .iter()
            .zip(&b.links)
            .all(|(a, b)| attrs_eq(&a.attrs, &b.attrs))
}
