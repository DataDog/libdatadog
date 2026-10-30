// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_common::regex_engine::{Regex, Replacer};
use libdd_trace_model::{
    AttributeValue, Attributes, AttributesMut, PROMOTED_SPAN_TAGS, SpanMut, TraceChunkMut,
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
    // * "resource.name" will target the resource
    // * "*" will target all tags and the resource
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
/// don't allocate. Like the Datadog Agent, `"*"` rules skip hidden (`_`-prefixed) tags; named rules
/// still apply to them.
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
                    if !is_hidden_tag(key.as_str()) {
                        apply_rule(rule, tag_value);
                    }
                }
                // The "*" wildcard intentionally applies to `span.resource` as well as
                // meta tags, matching the Datadog Agent reference implementation in
                // `pkg/trace/filters/replacer.go` (see the `Replace` and `ReplaceV1`
                // functions, which apply "*" rules to both span meta and `s.Resource`).
                apply_rule(rule, &mut span.resource);
            }
            "resource.name" => {
                apply_rule(rule, &mut span.resource);
            }
            _ => {
                if let Some(tag_value) = span.meta.get_mut(rule.name.as_str()) {
                    apply_rule(rule, tag_value);
                }
            }
        }
    }
}

/// Replaces the tag values of a span using the given rules, through the trace model traits.
///
/// Rules apply to the string leaves of attributes, including nested arrays and maps, and to
/// booleans in their `"true"`/`"false"` form. The promoted span tags (`env`, `version`,
/// `component`, `span.kind`), which v0.4 holds in `meta`, are reached through the
/// [`CompatSpan`](libdd_trace_model::CompatSpan) view, both by name and by `"*"` rules, matching
/// [`replace_span_tags_v04`]. Like a tag missing from v0.4 `meta`, an unset promoted tag is never
/// matched, even by a pattern matching the empty string. A replaced `span.kind` that isn't a known
/// kind unsets it. Like the Datadog Agent, `"*"` rules skip hidden (`_`-prefixed) attributes.
pub fn replace_span_tags_trait<S: SpanMut>(span: &mut S, rules: &[ReplaceRule]) {
    for rule in rules {
        match rule.name.as_ref() {
            "*" => {
                span.retain_attributes(|key, value| {
                    if !is_hidden_tag(key) {
                        apply_rule_to_attr_value(rule, value);
                    }
                    true
                });
                // The "*" wildcard intentionally applies to `span.resource` as well as
                // meta tags, matching the Datadog Agent reference implementation in
                // `pkg/trace/filters/replacer.go` (see the `Replace` and `ReplaceV1`
                // functions, which apply "*" rules to both span meta and `s.Resource`).
                replace_resource(rule, span);
                for key in PROMOTED_SPAN_TAGS {
                    replace_promoted_tag(rule, span, key);
                }
            }
            "resource.name" => replace_resource(rule, span),
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
/// attributes. Like the Datadog Agent, the chunk fields (e.g. the origin) are left untouched.
pub fn replace_chunk_tags<C: TraceChunkMut>(chunk: &mut C, rules: &[ReplaceRule]) {
    for rule in rules {
        match rule.name.as_ref() {
            "*" => chunk.retain_attributes(|key, value| {
                if !is_hidden_tag(key) {
                    apply_rule_to_attr_value(rule, value);
                }
                true
            }),
            "resource.name" => {}
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
fn is_hidden_tag(key: &str) -> bool {
    key.starts_with('_')
}

fn replace_resource<S: SpanMut>(rule: &ReplaceRule, span: &mut S) {
    if let Some(new) = replace_all_opt(&rule.re, &rule.repl, rule.no_expansion, span.resource()) {
        span.set_resource(S::Text::from(new));
    }
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

/// Applies `rule` to `value`, recursing into `Array`/`KeyValueList` so nested string leaves are
/// scrubbed too: the agentless encoder flattens those into plain dotted tags before sending, so
/// leaving them out here would let matching data (e.g. a `payment: {number: "..."}` attribute)
/// reach the intake unredacted.
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
        AttributeValue::Int(_) | AttributeValue::Float(_) | AttributeValue::Bytes(_) => {}
    }
}

/// `replace_span_tags` replaces the tag values of a span with a given set of rules.
///
/// Like the Datadog Agent, `"*"` rules skip hidden (`_`-prefixed) tags; named rules still apply to
/// them.
pub fn replace_span_tags(span: &mut pb::Span, rules: &[ReplaceRule], scratch_space: &mut String) {
    for rule in rules {
        match rule.name.as_ref() {
            "*" => {
                for (key, tag_value) in &mut span.meta {
                    if !is_hidden_tag(key) {
                        rule.apply(tag_value, scratch_space);
                    }
                }
                // The "*" wildcard intentionally applies to `span.resource` as well as
                // meta tags, matching the Datadog Agent reference implementation in
                // `pkg/trace/filters/replacer.go` (see the `Replace` and `ReplaceV1`
                // functions, which apply "*" rules to both span meta and `s.Resource`).
                rule.apply(&mut span.resource, scratch_space);
            }
            "resource.name" => {
                rule.apply(&mut span.resource, scratch_space);
            }
            _ => {
                if let Some(tag_value) = span.meta.get_mut(&rule.name) {
                    rule.apply(tag_value, scratch_space);
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
    use libdd_trace_model::{Attributes as _, AttributesIter as _, Span as _, TraceChunk as _};
    use libdd_trace_protobuf::pb;
    use libdd_trace_utils::span::{BytesData, v1, v04};
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
        span.attributes.insert(bs("env"), str_attr("stale-secret"));
        replacer::replace_span_tags_v1(&mut span, &rules);
        assert_eq!(
            (
                span.env(),
                span.version(),
                span.component(),
                span.span_kind()
            ),
            (
                Some("env-redacted"),
                Some("1.0-redacted"),
                Some("comp-redacted"),
                Some("client")
            )
        );
        // The replaced field is left as the only value.
        assert!(!span.has_attribute("env"));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn named_rules_replace_promoted_span_tag_fields_v1() {
        let rules = rules(
            r#"[
                {"name": "env", "pattern": "prod", "repl": "staging"},
                {"name": "span.kind", "pattern": "server", "repl": "custom"}
            ]"#,
        );
        let mut span = v1::SpanBytes {
            env: bs("prod"),
            span_kind: v1::SpanKind::Server,
            ..Default::default()
        };
        replacer::replace_span_tags_v1(&mut span, &rules);
        assert_eq!(span.env(), Some("staging"));
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
                {"name": "env", "pattern": "^$", "repl": "none"},
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
        span.attributes.insert(bs("env"), str_attr("secret"));
        replacer::replace_span_tags_v1(&mut span, &rules);
        // The field stays unset, and the plain attribute is scrubbed like any other.
        assert_eq!(span.env(), None);
        assert_eq!(span.attribute_str("env"), Some("?"));
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
}
