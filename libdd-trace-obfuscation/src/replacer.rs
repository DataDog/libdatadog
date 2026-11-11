// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::fmt::{self, Display, Write as _};
use libdd_regex::{Regex, Replacer};
use libdd_trace_model::{
    AttributeValue, Attributes, AttributesMut, ENV_KEY, PROMOTED_SPAN_TAGS, SpanMut, TraceChunkMut,
    TraceText, ValueArrayMut, ValueMapMut, ValueTypes,
};
use libdd_trace_protobuf::pb;
use libdd_trace_utils::span::{SpanText, TraceData, v1, v04};
use serde::{Deserialize, Deserializer, Serialize};

// Agent-facing representation. `re` and `no_expansion` are derived runtime state.
#[derive(Serialize, Deserialize)]
struct RawReplaceRule {
    name: String,
    pattern: String,
    repl: String,
}

impl PartialEq for ReplaceRule {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name && self.repl == other.repl && self.re.as_str() == other.re.as_str()
    }
}

#[derive(Debug, Clone)]
pub struct ReplaceRule {
    // name specifies the name of the tag that the replace rule addresses. However,
    // some exceptions apply such as:
    // * "resource.name" will target the resource
    // * "*" will target all tags and the resource, except hidden (`_`-prefixed) tags and `env`
    // * "env" rules are ignored: like the Datadog Agent, which drops them when loading its
    //   configuration, the env is never rewritten since it can be used for sampling and stats
    pub name: String,

    // re holds the regex pattern for matching.
    pub re: Regex,

    // repl specifies the replacement string to be used when Pattern matches.
    pub repl: String,

    // does the replacement pattern contain references to the capture groups
    pub no_expansion: bool,
}

impl<'de> Deserialize<'de> for ReplaceRule {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawReplaceRule::deserialize(deserializer)?;
        let re = Regex::new(&raw.pattern).map_err(serde::de::Error::custom)?;
        let no_expansion = Replacer::no_expansion(&mut raw.repl.as_str()).is_some();
        Ok(Self {
            name: raw.name,
            re,
            repl: raw.repl,
            no_expansion,
        })
    }
}

impl Serialize for ReplaceRule {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        RawReplaceRule {
            name: self.name.clone(),
            pattern: self.re.as_str().to_owned(),
            repl: self.repl.clone(),
        }
        .serialize(serializer)
    }
}

impl ReplaceRule {
    fn apply(&self, tag_value: &mut String, scratch_space: &mut String) {
        replace_all(
            &self.re,
            &self.repl,
            self.no_expansion,
            tag_value,
            scratch_space,
        );
    }
}

/// `replace_trace_tags` replaces the tag values of all spans within a trace with a given set of
/// rules.
pub fn replace_trace_tags(trace: &mut [pb::Span], rules: &[ReplaceRule]) {
    let mut scratch_space = String::new();
    for span in trace.iter_mut() {
        replace_span_tags(span, rules, &mut scratch_space);
    }
}

/// Replaces the tag values of a [`v04::Span`] using the given rules.
///
/// Fields are the immutable [`SpanText`] type, so matches are written back through
/// [`SpanText::from_owned`]. `replace_all_opt` returns `None` on no match, so untouched fields
/// don't allocate. Like the Datadog Agent, `"*"` rules skip hidden (`_`-prefixed) tags and `env`;
/// named rules still apply to hidden tags, and rules named `env` are ignored. Rules also apply to
/// `metrics`, see [`apply_rule_to_metric`].
pub fn replace_span_tags_v04<T: TraceData>(span: &mut v04::Span<T>, rules: &[ReplaceRule]) {
    fn apply_rule<S: SpanText>(rule: &ReplaceRule, field: &mut S) {
        if let Some(new) = replace_all_opt(&rule.re, &rule.repl, rule.no_expansion, field.borrow())
        {
            *field = S::from_owned(new);
        }
    }

    for rule in rules {
        match rule.name.as_ref() {
            "*" => {
                for (key, tag_value) in &mut span.meta {
                    if is_wildcard_replaceable_tag(key.as_str()) {
                        apply_rule(rule, tag_value);
                    }
                }
                let meta = &mut span.meta;
                span.metrics.retain_mut(|key, value| {
                    if !is_wildcard_replaceable_tag(key.as_str()) {
                        return true;
                    }
                    let Some(text) = apply_rule_to_metric(rule, value) else {
                        return true;
                    };
                    meta.insert_or_replace(
                        T::Text::from_owned(key.as_str().to_owned()),
                        T::Text::from_owned(text),
                    );
                    false
                });
                // The "*" wildcard intentionally applies to `span.resource` as well as
                // meta tags, matching the Datadog Agent reference implementation in
                // `pkg/trace/filters/replacer.go` (see the `Replace` and `ReplaceV1`
                // functions, which apply "*" rules to both span meta and `s.Resource`).
                apply_rule(rule, &mut span.resource);
            }
            "resource.name" => {
                apply_rule(rule, &mut span.resource);
            }
            ENV_KEY => {}
            name => {
                if let Some(tag_value) = span.meta.get_mut(name) {
                    apply_rule(rule, tag_value);
                }
                if let Some(text) = span
                    .metrics
                    .get_mut(name)
                    .and_then(|value| apply_rule_to_metric(rule, value))
                {
                    span.metrics.remove(name);
                    span.meta.insert_or_replace(
                        T::Text::from_owned(name.to_owned()),
                        T::Text::from_owned(text),
                    );
                }
            }
        }
    }
}

/// Replaces the tag values of a span using the given rules, through the trace model traits.
///
/// Rules apply to the string leaves of attributes, including nested arrays and maps, to booleans
/// in their `"true"`/`"false"` form and to numbers in their decimal form, see
/// [`apply_rule_to_attr_value`]. The promoted span tags (`version`, `component`, `span.kind`),
/// which v0.4 holds in `meta`, are reached through the
/// [`CompatSpan`](libdd_trace_model::CompatSpan) view, both by name and by `"*"` rules, matching
/// [`replace_span_tags_v04`]. Like a tag missing from v0.4 `meta`, an unset promoted tag is never
/// matched, even by a pattern matching the empty string. A replaced `span.kind` that isn't a known
/// kind unsets it. Like the Datadog Agent, `"*"` rules skip hidden (`_`-prefixed) attributes and
/// the env, and rules named `env` are ignored.
pub fn replace_span_tags_trait<S: SpanMut>(span: &mut S, rules: &[ReplaceRule]) {
    for rule in rules {
        match rule.name.as_ref() {
            "*" => {
                replace_wildcard_attributes(span, rule);
                // The "*" wildcard intentionally applies to `span.resource` as well as
                // meta tags, matching the Datadog Agent reference implementation in
                // `pkg/trace/filters/replacer.go` (see the `Replace` and `ReplaceV1`
                // functions, which apply "*" rules to both span meta and `s.Resource`).
                replace_resource(rule, span);
                for key in PROMOTED_SPAN_TAGS {
                    if is_wildcard_replaceable_tag(key) {
                        replace_promoted_tag(rule, span, key);
                    }
                }
            }
            "resource.name" => replace_resource(rule, span),
            ENV_KEY => {}
            name => match PROMOTED_SPAN_TAGS.into_iter().find(|key| *key == name) {
                Some(key) => replace_promoted_tag(rule, span, key),
                None => {
                    if let Some(value) = span.attribute_mut(name) {
                        apply_rule_to_attr_value(rule, value);
                    }
                }
            },
        }
    }
}

/// Replaces the tag values of a [`v1::Span`] using the given rules, see
/// [`replace_span_tags_trait`].
pub fn replace_span_tags_v1<T: TraceData>(span: &mut v1::Span<T>, rules: &[ReplaceRule]) {
    replace_span_tags_trait(span, rules);
}

/// Replaces the values of a trace chunk's own attributes using the given rules.
///
/// Chunk-level attributes are merged into every emitted span at encode time (see
/// `collect_attrs_v1` in the agentless encoder), so they need the same scrubbing as span-level
/// attributes. Like the Datadog Agent, the chunk fields (e.g. the origin) are left untouched. As
/// for spans, `"*"` rules skip hidden attributes and `env`, and rules named `env` are ignored.
pub fn replace_chunk_tags<C: TraceChunkMut>(chunk: &mut C, rules: &[ReplaceRule]) {
    for rule in rules {
        match rule.name.as_ref() {
            "*" => replace_wildcard_attributes(chunk, rule),
            "resource.name" | ENV_KEY => {}
            name => {
                if let Some(value) = chunk.attribute_mut(name) {
                    apply_rule_to_attr_value(rule, value);
                }
            }
        }
    }
}

/// Replaces the values of a [`v1::TraceChunk`]'s own attributes using the given rules, see
/// [`replace_chunk_tags`].
pub fn replace_chunk_tags_v1<T: TraceData>(chunk: &mut v1::TraceChunk<T>, rules: &[ReplaceRule]) {
    replace_chunk_tags(chunk, rules);
}

/// Whether `key` is a hidden (`_`-prefixed) tag, which `"*"` rules skip, like the Datadog Agent.
pub(crate) fn is_hidden_tag(key: &str) -> bool {
    key.starts_with('_')
}

/// Whether `"*"` rules may rewrite the tag `key`: like the Datadog Agent, they skip hidden tags
/// and `env`, since the env can be used for sampling and stats.
fn is_wildcard_replaceable_tag(key: &str) -> bool {
    key != ENV_KEY && !is_hidden_tag(key)
}

/// Applies `rule` to every attribute `"*"` rules may rewrite, see [`is_wildcard_replaceable_tag`].
///
/// Shared by the span and chunk `"*"` passes, which differ only in the container they walk.
fn replace_wildcard_attributes<A: AttributesMut>(attrs: &mut A, rule: &ReplaceRule) {
    attrs.retain_attributes(|key, value| {
        if is_wildcard_replaceable_tag(key) {
            apply_rule_to_attr_value(rule, value);
        }
        true
    });
}

/// Runs an obfuscator over the span's resource, writing it back only on a hit.
pub(crate) fn apply_resource<S: SpanMut>(span: &mut S, f: impl FnOnce(&str) -> Option<String>) {
    if let Some(new) = f(span.resource()) {
        span.set_resource(S::Text::from(new));
    }
}

fn replace_resource<S: SpanMut>(rule: &ReplaceRule, span: &mut S) {
    apply_resource(span, |resource| {
        replace_all_opt(&rule.re, &rule.repl, rule.no_expansion, resource)
    });
}

/// Applies `rule` to the promoted span tag `key`, read and written by name through the compat
/// view so that the dedicated field is the one replaced.
fn replace_promoted_tag<S: SpanMut>(rule: &ReplaceRule, span: &mut S, key: &'static str) {
    let Some(new) = span
        .compat()
        .attribute_str(key)
        .and_then(|value| replace_all_opt(&rule.re, &rule.repl, rule.no_expansion, value))
    else {
        return;
    };
    span.compat_mut().set_attribute(
        S::Text::from_static(key),
        AttributeValue::String(S::Text::from(new)),
    );
}

/// Applies `rule` to `value`, recursing into `Array`/`KeyValueList` so nested leaves are scrubbed
/// too: the agentless encoder flattens those into plain dotted tags before sending, so leaving
/// them out here would let matching data (e.g. a `payment: {number: "..."}` attribute) reach the
/// intake unredacted.
///
/// Numbers are matched in their decimal form, see [`with_decimal`]. Like the Datadog Agent, a
/// replaced number stays a number when the result parses as one (an integer first, then a float)
/// and becomes a string otherwise, so it moves from `metrics` to `meta` on the wire. `Bytes` are
/// left untouched: they hold msgpack-encoded `meta_struct` payloads, which a textual replacement
/// would corrupt.
fn apply_rule_to_attr_value<V: ValueTypes>(rule: &ReplaceRule, value: &mut AttributeValue<V>) {
    match value {
        AttributeValue::String(s) => {
            if let Some(new) = replace_all_opt(&rule.re, &rule.repl, rule.no_expansion, s.as_str())
            {
                *s = V::Text::from(new);
            }
        }
        AttributeValue::Array(values) => {
            for v in ValueArrayMut::iter_mut(values) {
                apply_rule_to_attr_value(rule, v);
            }
        }
        AttributeValue::KeyValueList(map) => {
            for (_, v) in ValueMapMut::iter_mut(map) {
                apply_rule_to_attr_value(rule, v);
            }
        }
        // `flatten_attr_into_v1` serializes bools as the literal strings "true"/"false" into
        // `meta`, so a rule must be able to match that textual form; only convert to `String` on
        // an actual match, to leave a non-matching value exactly as `flatten_attr_into_v1` would
        // have serialized it anyway.
        AttributeValue::Bool(b) => {
            let s = if *b { "true" } else { "false" };
            if let Some(new) = replace_all_opt(&rule.re, &rule.repl, rule.no_expansion, s) {
                *value = AttributeValue::String(V::Text::from(new));
            }
        }
        AttributeValue::Int(i) => {
            if let Some(new) = with_decimal(*i, |s| {
                replace_all_opt(&rule.re, &rule.repl, rule.no_expansion, s)
            }) {
                *value = numeric_attr_from_string(new);
            }
        }
        AttributeValue::Float(f) => {
            if let Some(new) = with_decimal(*f, |s| {
                replace_all_opt(&rule.re, &rule.repl, rule.no_expansion, s)
            }) {
                *value = numeric_attr_from_string(new);
            }
        }
        AttributeValue::Bytes(_) => {}
    }
}

/// The value a replaced number is written back as: like the Agent's `SetAttributeFromString`, an
/// integer if `s` parses as one, else a float if it parses as one, else a string.
fn numeric_attr_from_string<V: ValueTypes>(s: String) -> AttributeValue<V> {
    if let Ok(i) = s.parse::<i64>() {
        AttributeValue::Int(i)
    } else if let Ok(f) = s.parse::<f64>() {
        AttributeValue::Float(f)
    } else {
        AttributeValue::String(V::Text::from(s))
    }
}

/// Applies `rule` to a v0.4/protobuf metric, like the Agent's `replaceNumericTag`: the value is
/// matched in its decimal form (see [`with_decimal`]) and stays a metric if the result parses as
/// a float. Otherwise the result is returned: the caller must move it to `meta` under the same
/// key and remove the metric.
fn apply_rule_to_metric(rule: &ReplaceRule, value: &mut f64) -> Option<String> {
    let new = with_decimal(*value, |s| {
        replace_all_opt(&rule.re, &rule.repl, rule.no_expansion, s)
    })?;
    match new.parse::<f64>() {
        Ok(f) => {
            *value = f;
            None
        }
        Err(_) => Some(new),
    }
}

/// Runs `f` on the decimal form of `value`, formatted without allocating in the common case.
///
/// For finite numbers, `Display` produces the same text as the Agent's `strconv.FormatInt(v, 10)`
/// and `strconv.FormatFloat(v, 'f', -1, 64)`: the shortest round-tripping digits, never in
/// exponent form. Infinities differ (`inf` instead of `+Inf`). Floats too long for the stack
/// buffer (e.g. `1e300`) fall back to a heap allocation.
fn with_decimal<R>(value: impl Display, f: impl FnOnce(&str) -> R) -> R {
    struct StackBuf {
        buf: [u8; 64],
        len: usize,
    }
    impl fmt::Write for StackBuf {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let end = self.len.checked_add(s.len()).ok_or(fmt::Error)?;
            let dst = self.buf.get_mut(self.len..end).ok_or(fmt::Error)?;
            dst.copy_from_slice(s.as_bytes());
            self.len = end;
            Ok(())
        }
    }

    let mut stack = StackBuf {
        buf: [0; 64],
        len: 0,
    };
    if write!(stack, "{value}").is_ok()
        // Only whole `&str`s are copied in, so this can't fail.
        && let Some(s) = stack
            .buf
            .get(..stack.len)
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
    {
        return f(s);
    }
    f(&value.to_string())
}

/// `replace_span_tags` replaces the tag values of a span with a given set of rules.
///
/// Like the Datadog Agent, `"*"` rules skip hidden (`_`-prefixed) tags and `env`; named rules still
/// apply to hidden tags, and rules named `env` are ignored. Rules also apply to `metrics`, see
/// [`apply_rule_to_metric`].
pub fn replace_span_tags(span: &mut pb::Span, rules: &[ReplaceRule], scratch_space: &mut String) {
    for rule in rules {
        match rule.name.as_ref() {
            "*" => {
                for (key, tag_value) in &mut span.meta {
                    if is_wildcard_replaceable_tag(key) {
                        rule.apply(tag_value, scratch_space);
                    }
                }
                let meta = &mut span.meta;
                span.metrics.retain(|key, value| {
                    if !is_wildcard_replaceable_tag(key) {
                        return true;
                    }
                    let Some(text) = apply_rule_to_metric(rule, value) else {
                        return true;
                    };
                    meta.insert(key.clone(), text);
                    false
                });
                // The "*" wildcard intentionally applies to `span.resource` as well as
                // meta tags, matching the Datadog Agent reference implementation in
                // `pkg/trace/filters/replacer.go` (see the `Replace` and `ReplaceV1`
                // functions, which apply "*" rules to both span meta and `s.Resource`).
                rule.apply(&mut span.resource, scratch_space);
            }
            "resource.name" => {
                rule.apply(&mut span.resource, scratch_space);
            }
            ENV_KEY => {}
            name => {
                if let Some(tag_value) = span.meta.get_mut(name) {
                    rule.apply(tag_value, scratch_space);
                }
                if let Some(text) = span
                    .metrics
                    .get_mut(name)
                    .and_then(|value| apply_rule_to_metric(rule, value))
                {
                    span.metrics.remove(name);
                    span.meta.insert(rule.name.clone(), text);
                }
            }
        }
    }
}

/// `parse_rules_from_string` takes an array of rules, represented as an array of length 3 arrays
/// holding the tag name, regex pattern, and replacement string as strings.
/// * returns a vec of `ReplaceRules`
///
/// # Errors
///
/// Returns an error when the input is not valid JSON or a rule pattern is not a valid regex.
pub fn parse_rules_from_string(
    // rules: &'a [[&'a str; 3]],
    rules: &str,
) -> anyhow::Result<Vec<ReplaceRule>> {
    let raw_rules = serde_json::from_str::<Vec<RawReplaceRule>>(rules)?;

    let mut vec: Vec<ReplaceRule> = Vec::with_capacity(rules.len());

    // for [name, pattern, repl] in rules {
    for raw_rule in raw_rules {
        let compiled_regex = match Regex::new(&raw_rule.pattern) {
            Ok(res) => res,
            Err(err) => {
                anyhow::bail!("Obfuscator Error: Error while parsing rule: {err}")
            }
        };
        let no_expansion = Replacer::no_expansion(&mut &raw_rule.repl).is_some();
        vec.push(ReplaceRule {
            name: raw_rule.name,
            re: compiled_regex,
            repl: raw_rule.repl,
            no_expansion,
        });
    }
    Ok(vec)
}

/// Mutate the haystack by changing all occurences of the regex by the `replace` parameter
/// using the scratch space provided
///
/// Taken from `regex::replacen` to use a reusable scratch space instead of allocating a new String
/// <https://docs.rs/regex/1.10.2/src/regex/regex/string.rs.html#890-944>
fn replace_all(
    re: &Regex,
    mut replace: &str,
    no_expansion: bool,
    haystack: &mut String,
    scratch_space: &mut String,
) {
    // If we know that the replacement doesn't have any capture expansions,
    // then we can use the fast path. The fast path can make a tremendous
    // difference:
    //
    //   1) We use `find_iter` instead of `captures_iter`. Not asking for captures generally makes
    //      the regex engines faster.
    //   2) We don't need to look up all of the capture groups and do replacements inside the
    //      replacement string. We just push it at each match and be done with it.
    if no_expansion {
        let mut it = re.find_iter(haystack).peekable();
        if it.peek().is_none() {
            return;
        }
        scratch_space.reserve(haystack.len());
        let mut last_match = 0;
        for m in it {
            scratch_space.push_str(&haystack[last_match..m.start()]);
            scratch_space.push_str(replace);
            last_match = m.end();
        }
        scratch_space.push_str(&haystack[last_match..]);
    } else {
        // The slower path, which we use if the replacement may need access to
        // capture groups.
        let mut it = re.captures_iter(haystack).peekable();
        if it.peek().is_none() {
            return;
        }
        scratch_space.reserve(haystack.len());
        let mut last_match = 0;
        for cap in it {
            // unwrap on 0 is OK because captures only reports matches
            #[allow(clippy::unwrap_used)]
            let m = cap.get(0).unwrap();
            scratch_space.push_str(&haystack[last_match..m.start()]);
            Replacer::replace_append(&mut replace, &cap, scratch_space);
            last_match = m.end();
        }
        scratch_space.push_str(&haystack[last_match..]);
    }
    core::mem::swap(scratch_space, haystack);
    scratch_space.clear();
}

/// Variant of [`replace_all`] for callers holding an immutable `&str` (e.g. [`SpanText`]-backed
/// spans). Returns `None` when the regex doesn't match `haystack`.
fn replace_all_opt(
    re: &Regex,
    mut replace: &str,
    no_expansion: bool,
    haystack: &str,
) -> Option<String> {
    if no_expansion {
        let mut it = re.find_iter(haystack).peekable();
        it.peek()?;
        let mut out = String::with_capacity(haystack.len());
        let mut last_match = 0;
        for m in it {
            out.push_str(&haystack[last_match..m.start()]);
            out.push_str(replace);
            last_match = m.end();
        }
        out.push_str(&haystack[last_match..]);
        Some(out)
    } else {
        let mut it = re.captures_iter(haystack).peekable();
        it.peek()?;
        let mut out = String::with_capacity(haystack.len());
        let mut last_match = 0;
        for cap in it {
            // unwrap on 0 is OK because captures only reports matches
            #[allow(clippy::unwrap_used)]
            let m = cap.get(0).unwrap();
            out.push_str(&haystack[last_match..m.start()]);
            Replacer::replace_append(&mut replace, &cap, &mut out);
            last_match = m.end();
        }
        out.push_str(&haystack[last_match..]);
        Some(out)
    }
}

#[cfg(test)]
mod tests {

    use super::Regex;
    use crate::replacer;
    use duplicate::duplicate_item;
    use libdd_tinybytes::BytesString;
    use libdd_trace_model::{Attributes as _, Span as _, TraceChunk as _};
    use libdd_trace_protobuf::pb;
    use libdd_trace_utils::span::{BytesData, v1, v04, vec_map::VecMap};
    use std::collections::HashMap;

    fn new_test_span_with_tags(tags: HashMap<&str, &str>) -> pb::Span {
        let mut span = pb::Span {
            duration: 10000000,
            error: 0,
            resource: "GET /some/raclette".to_string(),
            service: "django".to_string(),
            name: "django.controller".to_string(),
            span_id: 123,
            start: 1448466874000000000,
            trace_id: 424242,
            meta: HashMap::new(),
            metrics: HashMap::from([("cheese_weight".to_string(), 100000.0)]),
            parent_id: 1111,
            r#type: "http".to_string(),
            meta_struct: HashMap::new(),
            span_links: vec![],
            span_events: vec![],
        };
        for (key, val) in tags {
            match key {
                "resource.name" => {
                    span.resource = val.to_string();
                }
                _ => {
                    span.meta.insert(key.to_string(), val.to_string());
                }
            }
        }
        span
    }

    #[duplicate_item(
    test_name rules input expected;
    [test_replace_tags] [r#"[
                        {"name": "http.url", "pattern": "(token/)([^/]*)", "repl": "${1}?"},
                        {"name": "http.url", "pattern": "guid", "repl": "[REDACTED]"},
                        {"name": "custom.tag", "pattern": "(/foo/bar/).*", "repl": "${1}extra"}
                    ]"#] [
                        HashMap::from([
                            ("http.url", "some/guid/token/abcdef/abc"),
                            ("custom.tag", "/foo/bar/foo"),
                        ])
                    ] [
                        HashMap::from([
                            ("http.url", "some/[REDACTED]/token/?/abc"),
                            ("custom.tag", "/foo/bar/extra"),
                        ])
                    ];
    [test_replace_tags_with_exceptions] [r#"[
                        {"name": "*", "pattern": "(token/)([^/]*)", "repl": "${1}?"},
                        {"name": "*", "pattern": "this", "repl": "that"},
                        {"name": "http.url", "pattern": "guid", "repl": "[REDACTED]"},
                        {"name": "custom.tag", "pattern": "(/foo/bar/).*", "repl": "${1}extra"},
                        {"name": "resource.name", "pattern": "prod", "repl": "stage"}
                    ]"#] [
                        HashMap::from([
                            ("resource.name", "this is prod"),
                            ("http.url", "some/[REDACTED]/token/abcdef/abc"),
                            ("other.url", "some/guid/token/abcdef/abc"),
                            ("custom.tag", "/foo/bar/foo"),
                        ])
                    ] [
                        HashMap::from([
                            ("resource.name", "that is stage"),
                            ("http.url", "some/[REDACTED]/token/?/abc"),
                            ("other.url", "some/guid/token/?/abc"),
                            ("custom.tag", "/foo/bar/extra"),
                        ])
                    ];
    )]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_name() {
        let parsed_rules = replacer::parse_rules_from_string(rules);

        let root_span = new_test_span_with_tags(input);
        let child_span = new_test_span_with_tags(input);
        let mut trace = [root_span, child_span];

        replacer::replace_trace_tags(&mut trace, &parsed_rules.unwrap());

        for (key, val) in expected {
            if key == "resource.name" {
                assert_eq!(val, trace[0].resource);
                assert_eq!(val, trace[1].resource);
            } else {
                assert_eq!(val, trace[0].meta.get(key).unwrap());
                assert_eq!(val, trace[1].meta.get(key).unwrap());
            }
        }
    }

    #[test]
    fn test_parse_rules_invalid_regex() {
        let result = replacer::parse_rules_from_string(r#"[{"http.url", ")", "${1}?"}]"#);
        assert!(result.is_err());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_replace_rule_serde_uses_agent_fields() {
        let agent_rules = serde_json::json!([{
            "name": "http.url",
            "pattern": "(token/)([^/]*)",
            "repl": "${1}?"
        }]);

        let rules: Vec<replacer::ReplaceRule> =
            serde_json::from_value(agent_rules.clone()).unwrap();
        let serialized = serde_json::to_value(&rules).unwrap();

        assert_eq!(serialized, agent_rules);
        assert_eq!(
            serde_json::from_value::<Vec<replacer::ReplaceRule>>(serialized).unwrap(),
            rules
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_replace_rule_eq() {
        let rule1 = replacer::ReplaceRule {
            name: "http.url".to_string(),
            re: Regex::new("(token/)([^/]*)").unwrap(),
            repl: "${1}?".to_string(),
            no_expansion: false,
        };
        let rule2 = replacer::ReplaceRule {
            name: "http.url".to_string(),
            re: Regex::new("(token/)([^/]*)").unwrap(),
            repl: "${1}?".to_string(),
            no_expansion: false,
        };
        assert_eq!(rule1, rule2);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_replace_rule_neq() {
        let rule1 = replacer::ReplaceRule {
            name: "http.url".to_string(),
            re: Regex::new("(token/)([^/]*)").unwrap(),
            repl: "${1}?".to_string(),
            no_expansion: false,
        };
        let rule2 = replacer::ReplaceRule {
            name: "http.url".to_string(),
            re: Regex::new("(broken/)([^/]*)").unwrap(),
            repl: "${1}?".to_string(),
            no_expansion: false,
        };
        assert_ne!(rule1, rule2);
    }

    fn bs(s: &str) -> BytesString {
        BytesString::from_string(s.to_owned())
    }

    fn str_attr(s: &str) -> v1::AttributeValue<BytesData> {
        v1::AttributeValue::String(bs(s))
    }

    fn rules(json: &str) -> Vec<replacer::ReplaceRule> {
        replacer::parse_rules_from_string(json).unwrap()
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn star_rule_skips_hidden_tags_in_every_format() {
        let rules = rules(r#"[{"name": "*", "pattern": "secret", "repl": "?"}]"#);

        let mut pb_span = new_test_span_with_tags(HashMap::from([
            ("_dd.hidden", "secret"),
            ("visible", "secret"),
        ]));
        replacer::replace_trace_tags(core::slice::from_mut(&mut pb_span), &rules);
        assert_eq!(pb_span.meta["_dd.hidden"], "secret");
        assert_eq!(pb_span.meta["visible"], "?");

        let mut v04_span = v04::SpanBytes::default();
        v04_span.meta.insert(bs("_dd.hidden"), bs("secret"));
        v04_span.meta.insert(bs("visible"), bs("secret"));
        replacer::replace_span_tags_v04(&mut v04_span, &rules);
        assert_eq!(v04_span.meta.get("_dd.hidden").unwrap().as_str(), "secret");
        assert_eq!(v04_span.meta.get("visible").unwrap().as_str(), "?");

        let mut v1_span = v1::SpanBytes::default();
        v1_span
            .attributes
            .insert(bs("_dd.hidden"), str_attr("secret"));
        v1_span.attributes.insert(bs("visible"), str_attr("secret"));
        replacer::replace_span_tags_v1(&mut v1_span, &rules);
        assert_eq!(v1_span.attribute_str("_dd.hidden"), Some("secret"));
        assert_eq!(v1_span.attribute_str("visible"), Some("?"));

        let mut chunk = v1::TraceChunkBytes::default();
        chunk
            .attributes
            .insert(bs("_dd.hidden"), str_attr("secret"));
        chunk.attributes.insert(bs("visible"), str_attr("secret"));
        replacer::replace_chunk_tags_v1(&mut chunk, &rules);
        assert_eq!(chunk.attribute_str("_dd.hidden"), Some("secret"));
        assert_eq!(chunk.attribute_str("visible"), Some("?"));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn named_rules_can_target_hidden_tags() {
        let rules = rules(r#"[{"name": "_dd.hidden", "pattern": "secret", "repl": "?"}]"#);
        let mut span = v1::SpanBytes::default();
        span.attributes.insert(bs("_dd.hidden"), str_attr("secret"));
        replacer::replace_span_tags_v1(&mut span, &rules);
        assert_eq!(span.attribute_str("_dd.hidden"), Some("?"));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn star_rule_replaces_promoted_span_tags_v1() {
        let rules = rules(
            r#"[
                {"name": "*", "pattern": "secret", "repl": "redacted"},
                {"name": "*", "pattern": "^server$", "repl": "client"}
            ]"#,
        );
        let mut span = v1::SpanBytes {
            env: bs("env-secret"),
            version: bs("1.0-secret"),
            component: bs("comp-secret"),
            span_kind: v1::SpanKind::Server,
            ..Default::default()
        };
        // A stale attribute named like a promoted tag, e.g. one the decoder didn't promote.
        span.attributes
            .insert(bs("version"), str_attr("stale-secret"));
        replacer::replace_span_tags_v1(&mut span, &rules);
        assert_eq!(
            (
                span.env(),
                span.version(),
                span.component(),
                span.span_kind()
            ),
            (
                // Like the Datadog Agent, "*" rules never rewrite the env.
                Some("env-secret"),
                Some("1.0-redacted"),
                Some("comp-redacted"),
                Some("client")
            )
        );
        // The replaced field is left as the only value.
        assert!(!span.has_attribute("version"));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn named_rules_replace_promoted_span_tag_fields_v1() {
        let rules = rules(
            r#"[
                {"name": "version", "pattern": "beta", "repl": "rc"},
                {"name": "span.kind", "pattern": "server", "repl": "custom"}
            ]"#,
        );
        let mut span = v1::SpanBytes {
            version: bs("1.0-beta"),
            span_kind: v1::SpanKind::Server,
            ..Default::default()
        };
        replacer::replace_span_tags_v1(&mut span, &rules);
        assert_eq!(span.version(), Some("1.0-rc"));
        // A span kind outside the OTel ones can't be stored: it reads as unset.
        assert_eq!(span.span_kind(), None);
        assert!(span.attributes.is_empty());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn star_rule_can_unset_the_span_kind_v1() {
        let rules = rules(r#"[{"name": "*", "pattern": "^client$", "repl": "custom"}]"#);
        let mut span = v1::SpanBytes {
            span_kind: v1::SpanKind::Client,
            ..Default::default()
        };
        replacer::replace_span_tags_v1(&mut span, &rules);
        assert_eq!(span.span_kind(), None);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn unset_promoted_tags_are_never_matched_v1() {
        // Like a tag missing from v0.4 `meta`, even a pattern matching the empty string.
        let rules = rules(
            r#"[
                {"name": "version", "pattern": "^$", "repl": "none"},
                {"name": "*", "pattern": "^$", "repl": "empty"}
            ]"#,
        );
        let mut span = v1::SpanBytes::default();
        replacer::replace_span_tags_v1(&mut span, &rules);
        assert_eq!(
            (
                span.env(),
                span.version(),
                span.component(),
                span.span_kind()
            ),
            (None, None, None, None)
        );
        assert_eq!(span.resource(), "empty");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn star_rule_scrubs_stale_promoted_attribute_when_field_is_unset_v1() {
        let rules = rules(r#"[{"name": "*", "pattern": "secret", "repl": "?"}]"#);
        let mut span = v1::SpanBytes::default();
        span.attributes.insert(bs("component"), str_attr("secret"));
        replacer::replace_span_tags_v1(&mut span, &rules);
        // The field stays unset, and the plain attribute is scrubbed like any other.
        assert_eq!(span.component(), None);
        assert_eq!(span.attribute_str("component"), Some("?"));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn env_is_never_rewritten_in_any_format() {
        // Like the Datadog Agent: "*" rules skip the env and rules named "env" are ignored, so
        // the env used for sampling and stats can't change. Other tags are still rewritten.
        let rules = rules(
            r#"[
                {"name": "*", "pattern": "secret", "repl": "?"},
                {"name": "*", "pattern": "^42$", "repl": "secret"},
                {"name": "env", "pattern": "secret", "repl": "?"}
            ]"#,
        );

        let mut pb_span =
            new_test_span_with_tags(HashMap::from([("env", "secret"), ("visible", "secret")]));
        pb_span.metrics.insert("env".to_owned(), 42.0);
        replacer::replace_trace_tags(core::slice::from_mut(&mut pb_span), &rules);
        assert_eq!(pb_span.meta["env"], "secret");
        assert_eq!(pb_span.meta["visible"], "?");
        assert_eq!(pb_span.metrics.get("env"), Some(&42.0));

        let mut v04_span = v04::SpanBytes::default();
        v04_span.meta.insert(bs("env"), bs("secret"));
        v04_span.meta.insert(bs("visible"), bs("secret"));
        v04_span.metrics.insert(bs("env"), 42.0);
        replacer::replace_span_tags_v04(&mut v04_span, &rules);
        assert_eq!(v04_span.meta.get("env").unwrap().as_str(), "secret");
        assert_eq!(v04_span.meta.get("visible").unwrap().as_str(), "?");
        assert_eq!(v04_span.metrics.get("env"), Some(&42.0));

        let mut v1_span = v1::SpanBytes {
            env: bs("secret"),
            ..Default::default()
        };
        // A stale attribute named like the env, e.g. one the decoder didn't promote.
        v1_span.attributes.insert(bs("env"), str_attr("secret"));
        v1_span.attributes.insert(bs("visible"), str_attr("secret"));
        replacer::replace_span_tags_v1(&mut v1_span, &rules);
        assert_eq!(v1_span.env(), Some("secret"));
        assert_eq!(v1_span.attributes.get("env"), Some(&str_attr("secret")));
        assert_eq!(v1_span.attribute_str("visible"), Some("?"));

        let mut chunk = v1::TraceChunkBytes::default();
        chunk.attributes.insert(bs("env"), str_attr("secret"));
        chunk.attributes.insert(bs("visible"), str_attr("secret"));
        replacer::replace_chunk_tags_v1(&mut chunk, &rules);
        assert_eq!(chunk.attribute_str("env"), Some("secret"));
        assert_eq!(chunk.attribute_str("visible"), Some("?"));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn rules_match_numeric_attributes_in_decimal_form_v1() {
        let rules = rules(
            r#"[
                {"name": "*", "pattern": "^4111111111111111$", "repl": "?"},
                {"name": "*", "pattern": "^42$", "repl": "43"},
                {"name": "*", "pattern": "5$", "repl": "25"},
                {"name": "ratio", "pattern": "^0\\.", "repl": "1."}
            ]"#,
        );
        let mut span = v1::SpanBytes::default();
        let attrs = [
            // A non-number result becomes a string.
            ("card", v1::AttributeValue::Int(4_111_111_111_111_111)),
            // A number result stays a number, re-typed from its text like the Agent does.
            ("count", v1::AttributeValue::Int(42)),
            ("latency", v1::AttributeValue::Float(1.5)),
            ("ratio", v1::AttributeValue::Float(0.75)),
            ("whole", v1::AttributeValue::Float(42.0)),
            // Nested numbers are rewritten too.
            (
                "list",
                v1::AttributeValue::Array(vec![v1::AttributeValue::Int(42)]),
            ),
            // Not a match.
            ("other", v1::AttributeValue::Int(7)),
        ];
        for (key, value) in attrs {
            span.attributes.insert(bs(key), value);
        }
        replacer::replace_span_tags_v1(&mut span, &rules);
        let get = |key: &str| span.attributes.get(key).cloned();
        assert_eq!(get("card"), Some(str_attr("?")));
        assert_eq!(get("count"), Some(v1::AttributeValue::Int(43)));
        assert_eq!(get("latency"), Some(v1::AttributeValue::Float(1.25)));
        // Rules apply in order: `0.75` -> `0.725` by the "5$" rule, then `1.725` by name.
        assert_eq!(get("ratio"), Some(v1::AttributeValue::Float(1.725)));
        // `42.0` is formatted as `42`, like Go's `FormatFloat(v, 'f', -1, 64)`.
        assert_eq!(get("whole"), Some(v1::AttributeValue::Int(43)));
        assert_eq!(
            get("list"),
            Some(v1::AttributeValue::Array(vec![v1::AttributeValue::Int(43)]))
        );
        assert_eq!(get("other"), Some(v1::AttributeValue::Int(7)));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn rules_leave_bytes_attributes_untouched_v1() {
        let rules = rules(r#"[{"name": "*", "pattern": "secret", "repl": "?"}]"#);
        let bytes = v1::AttributeValue::Bytes(libdd_tinybytes::Bytes::from_static(b"\xa6secret"));
        let mut span = v1::SpanBytes::default();
        span.attributes.insert(bs("blob"), bytes.clone());
        replacer::replace_span_tags_v1(&mut span, &rules);
        assert_eq!(span.attributes.get("blob"), Some(&bytes));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn rules_apply_to_metrics_like_the_agent() {
        let rules = rules(
            r#"[
                {"name": "*", "pattern": "^4111111111111111$", "repl": "?"},
                {"name": "*", "pattern": "^42$", "repl": "43"},
                {"name": "named", "pattern": "^7$", "repl": "seven"}
            ]"#,
        );
        let metrics = [
            // A non-number result moves to meta.
            ("card", 4_111_111_111_111_111.0),
            ("named", 7.0),
            // A number result stays a metric.
            ("count", 42.0),
            // Hidden metrics are skipped by "*".
            ("_dd.hidden", 42.0),
            ("other", 1.5),
        ];

        let mut pb_span = new_test_span_with_tags(HashMap::new());
        pb_span.metrics = metrics
            .iter()
            .map(|(key, value)| ((*key).to_owned(), *value))
            .collect();
        replacer::replace_trace_tags(core::slice::from_mut(&mut pb_span), &rules);
        assert_eq!(pb_span.meta.get("card").map(String::as_str), Some("?"));
        assert_eq!(pb_span.meta.get("named").map(String::as_str), Some("seven"));
        assert!(!pb_span.metrics.contains_key("card"));
        assert!(!pb_span.metrics.contains_key("named"));
        assert_eq!(pb_span.metrics.get("count"), Some(&43.0));
        assert_eq!(pb_span.metrics.get("_dd.hidden"), Some(&42.0));
        assert_eq!(pb_span.metrics.get("other"), Some(&1.5));

        let mut v04_span = v04::SpanBytes::default();
        for (key, value) in metrics {
            v04_span.metrics.insert(bs(key), value);
        }
        replacer::replace_span_tags_v04(&mut v04_span, &rules);
        let meta = |key: &str| v04_span.meta.get(key).map(BytesString::as_str);
        assert_eq!(meta("card"), Some("?"));
        assert_eq!(meta("named"), Some("seven"));
        assert_eq!(v04_span.metrics.get("card"), None);
        assert_eq!(v04_span.metrics.get("named"), None);
        assert_eq!(v04_span.metrics.get("count"), Some(&43.0));
        assert_eq!(v04_span.metrics.get("_dd.hidden"), Some(&42.0));
        assert_eq!(v04_span.metrics.get("other"), Some(&1.5));
    }

    #[test]
    fn decimal_form_matches_go_formatting() {
        let fmt = |v: f64| super::with_decimal(v, str::to_owned);
        assert_eq!(fmt(42.0), "42");
        assert_eq!(fmt(1.5), "1.5");
        assert_eq!(fmt(1e-7), "0.0000001");
        assert_eq!(fmt(4_111_111_111_111_111.0), "4111111111111111");
        // Too long for the stack buffer: falls back to the heap.
        assert_eq!(fmt(1e100), format!("1{}", "0".repeat(100)));
        assert_eq!(
            super::with_decimal(i64::MIN, str::to_owned),
            i64::MIN.to_string()
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn chunk_rules_leave_the_origin_field_untouched() {
        let rules = rules(
            r#"[
                {"name": "*", "pattern": "synthetics", "repl": "?"},
                {"name": "_dd.origin", "pattern": "synthetics", "repl": "?"}
            ]"#,
        );
        let mut chunk = v1::TraceChunkBytes {
            origin: bs("synthetics"),
            ..Default::default()
        };
        chunk.attributes.insert(bs("team"), str_attr("synthetics"));
        replacer::replace_chunk_tags_v1(&mut chunk, &rules);
        assert_eq!(chunk.origin(), "synthetics");
        assert_eq!(chunk.attribute_str("team"), Some("?"));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn chunk_rules_treat_attributes_named_like_promoted_chunk_tags_as_plain_attributes() {
        let star = rules(r#"[{"name": "*", "pattern": "secret", "repl": "?"}]"#);
        let named = rules(
            r#"[
                {"name": "_dd.origin", "pattern": "secret", "repl": "?"},
                {"name": "_sampling_priority_v1", "pattern": "secret", "repl": "?"},
                {"name": "_dd.p.dm", "pattern": "secret", "repl": "?"}
            ]"#,
        );
        let new_chunk = || {
            let mut chunk = v1::TraceChunkBytes {
                origin: bs("secret"),
                priority: Some(1),
                sampling_mechanism: Some(4),
                ..Default::default()
            };
            // `origin` isn't a promoted name: it is an ordinary attribute.
            chunk.attributes.insert(bs("origin"), str_attr("secret"));
            // Attributes named like the promoted chunk tags, e.g. ones a decoder didn't promote.
            for key in libdd_trace_model::PROMOTED_CHUNK_TAGS {
                chunk.attributes.insert(bs(key), str_attr("secret"));
            }
            chunk
        };
        let assert_fields_untouched = |chunk: &v1::TraceChunkBytes| {
            assert_eq!(chunk.origin(), "secret");
            assert_eq!(chunk.priority, Some(1));
            assert_eq!(chunk.sampling_mechanism, Some(4));
        };

        // "*" scrubs the ordinary `origin` attribute but skips the hidden promoted names.
        let mut chunk = new_chunk();
        replacer::replace_chunk_tags_v1(&mut chunk, &star);
        assert_fields_untouched(&chunk);
        assert_eq!(chunk.attributes.get("origin"), Some(&str_attr("?")));
        for key in libdd_trace_model::PROMOTED_CHUNK_TAGS {
            assert_eq!(
                chunk.attributes.get(key),
                Some(&str_attr("secret")),
                "{key}"
            );
        }

        // Named rules reach the plain attributes, never the fields.
        let mut chunk = new_chunk();
        replacer::replace_chunk_tags_v1(&mut chunk, &named);
        assert_fields_untouched(&chunk);
        assert_eq!(chunk.attributes.get("origin"), Some(&str_attr("secret")));
        for key in libdd_trace_model::PROMOTED_CHUNK_TAGS {
            assert_eq!(chunk.attributes.get(key), Some(&str_attr("?")), "{key}");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn named_rules_can_target_hidden_tags_in_every_format() {
        let rules = rules(r#"[{"name": "_dd.hidden", "pattern": "secret", "repl": "?"}]"#);

        let mut pb_span = new_test_span_with_tags(HashMap::from([("_dd.hidden", "secret")]));
        replacer::replace_trace_tags(core::slice::from_mut(&mut pb_span), &rules);
        assert_eq!(pb_span.meta["_dd.hidden"], "?");

        let mut v04_span = v04::SpanBytes::default();
        v04_span.meta.insert(bs("_dd.hidden"), bs("secret"));
        replacer::replace_span_tags_v04(&mut v04_span, &rules);
        assert_eq!(v04_span.meta.get("_dd.hidden").unwrap().as_str(), "?");

        let mut chunk = v1::TraceChunkBytes::default();
        chunk
            .attributes
            .insert(bs("_dd.hidden"), str_attr("secret"));
        replacer::replace_chunk_tags_v1(&mut chunk, &rules);
        assert_eq!(chunk.attribute_str("_dd.hidden"), Some("?"));
    }

    /// `[String, [String], Int]`: a leaf, a nested leaf and a non-string leaf.
    fn nested_array(s: &str) -> v1::AttributeValue<BytesData> {
        v1::AttributeValue::Array(vec![
            str_attr(s),
            v1::AttributeValue::Array(vec![str_attr(s)]),
            v1::AttributeValue::Bool(true),
        ])
    }

    /// `{inner: String, nested: {deep: String}}`.
    fn nested_map(s: &str) -> v1::AttributeValue<BytesData> {
        let mut deep = VecMap::new();
        deep.insert(bs("deep"), str_attr(s));
        v1::AttributeValue::KeyValueList(
            [
                (bs("inner"), str_attr(s)),
                (bs("nested"), v1::AttributeValue::KeyValueList(deep)),
            ]
            .into_iter()
            .collect(),
        )
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn rules_recurse_into_arrays_and_maps_v1() {
        for json in [
            r#"[{"name": "*", "pattern": "secret", "repl": "?"}]"#,
            r#"[
                {"name": "arr", "pattern": "secret", "repl": "?"},
                {"name": "map", "pattern": "secret", "repl": "?"}
            ]"#,
        ] {
            let rules = rules(json);
            let mut span = v1::SpanBytes::default();
            span.attributes.insert(bs("arr"), nested_array("secret"));
            span.attributes.insert(bs("map"), nested_map("secret"));
            replacer::replace_span_tags_v1(&mut span, &rules);
            assert_eq!(
                span.attributes.get("arr"),
                Some(&nested_array("?")),
                "{json}"
            );
            assert_eq!(span.attributes.get("map"), Some(&nested_map("?")), "{json}");

            let mut chunk = v1::TraceChunkBytes::default();
            chunk.attributes.insert(bs("arr"), nested_array("secret"));
            chunk.attributes.insert(bs("map"), nested_map("secret"));
            replacer::replace_chunk_tags_v1(&mut chunk, &rules);
            assert_eq!(
                chunk.attributes.get("arr"),
                Some(&nested_array("?")),
                "{json}"
            );
            assert_eq!(
                chunk.attributes.get("map"),
                Some(&nested_map("?")),
                "{json}"
            );
        }
    }
}
