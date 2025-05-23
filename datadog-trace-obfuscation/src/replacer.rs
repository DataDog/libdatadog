// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_trace_protobuf::pb;
use regex::Regex;
use serde::Deserialize;

#[derive(Deserialize)]
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
    name: String,

    // re holds the regex pattern for matching.
    re: regex::Regex,

    // repl specifies the replacement string to be used when Pattern matches.
    repl: String,

    // does the replacement pattern contain references to the capture groups
    no_expansion: bool,
}

impl ReplaceRule {
    fn apply(&self, tag_value: &mut String, scratch_space: &mut String) {
        replace_all(
            &self.re,
            &self.repl,
            self.no_expansion,
            tag_value,
            scratch_space,
        )
    }
}

/// replace_trace_tags replaces the tag values of all spans within a trace with a given set of
/// rules.
pub fn replace_trace_tags(trace: &mut [pb::Span], rules: &[ReplaceRule]) {
    let mut scratch_space = String::new();
    for span in trace.iter_mut() {
        replace_span_tags(span, rules, &mut scratch_space);
    }
}

/// replace_span_tags replaces the tag values of a span with a given set of rules.
pub fn replace_span_tags(span: &mut pb::Span, rules: &[ReplaceRule], scratch_space: &mut String) {
    for rule in rules {
        match rule.name.as_ref() {
            "*" => {
                for (_, tag_value) in span.meta.iter_mut() {
                    rule.apply(tag_value, scratch_space);
                }
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

/// parse_rules_from_string takes an array of rules, represented as an array of length 3 arrays
/// holding the tag name, regex pattern, and replacement string as strings.
/// * returns a vec of ReplaceRules
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
                anyhow::bail!("Obfuscator Error: Error while parsing rule: {}", err)
            }
        };
        let no_expansion = regex::Replacer::no_expansion(&mut &raw_rule.repl).is_some();
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
/// Taken from regex::replacen to use a reusable scratch space instead of allocating a new String
/// https://docs.rs/regex/1.10.2/src/regex/regex/string.rs.html#890-944
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
            regex::Replacer::replace_append(&mut replace, &cap, scratch_space);
            last_match = m.end();
        }
        scratch_space.push_str(&haystack[last_match..]);
    }
    std::mem::swap(scratch_space, haystack);
    scratch_space.truncate(0);
}

#[cfg(test)]
mod tests {

    use crate::replacer;
    use datadog_trace_protobuf::pb;
    use duplicate::duplicate_item;
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
        [
        test_name   [test_replace_tags]
        rules       [r#"[
                        {"name": "http.url", "pattern": "(token/)([^/]*)", "repl": "${1}?"},
                        {"name": "http.url", "pattern": "guid", "repl": "[REDACTED]"},
                        {"name": "custom.tag", "pattern": "(/foo/bar/).*", "repl": "${1}extra"}
                    ]"#]
        input       [
                        HashMap::from([
                            ("http.url", "some/guid/token/abcdef/abc"),
                            ("custom.tag", "/foo/bar/foo"),
                        ])
                    ]
        expected    [
                        HashMap::from([
                            ("http.url", "some/[REDACTED]/token/?/abc"),
                            ("custom.tag", "/foo/bar/extra"),
                        ])
                    ];
        ]
        [
        test_name   [test_replace_tags_with_exceptions]
        rules       [r#"[
                        {"name": "*", "pattern": "(token/)([^/]*)", "repl": "${1}?"},
                        {"name": "*", "pattern": "this", "repl": "that"},
                        {"name": "http.url", "pattern": "guid", "repl": "[REDACTED]"},
                        {"name": "custom.tag", "pattern": "(/foo/bar/).*", "repl": "${1}extra"},
                        {"name": "resource.name", "pattern": "prod", "repl": "stage"}
                    ]"#]
        input       [
                        HashMap::from([
                            ("resource.name", "this is prod"),
                            ("http.url", "some/[REDACTED]/token/abcdef/abc"),
                            ("other.url", "some/guid/token/abcdef/abc"),
                            ("custom.tag", "/foo/bar/foo"),
                        ])
                    ]
        expected    [
                        HashMap::from([
                            ("resource.name", "this is stage"),
                            ("http.url", "some/[REDACTED]/token/?/abc"),
                            ("other.url", "some/guid/token/?/abc"),
                            ("custom.tag", "/foo/bar/extra"),
                        ])
                    ];
        ]
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
            match key {
                "resource.name" => {
                    assert_eq!(val, trace[0].resource);
                    assert_eq!(val, trace[1].resource);
                }
                _ => {
                    assert_eq!(val, trace[0].meta.get(key).unwrap());
                    assert_eq!(val, trace[1].meta.get(key).unwrap());
                }
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
    fn test_replace_rule_eq() {
        let rule1 = replacer::ReplaceRule {
            name: "http.url".to_string(),
            re: regex::Regex::new("(token/)([^/]*)").unwrap(),
            repl: "${1}?".to_string(),
            no_expansion: false,
        };
        let rule2 = replacer::ReplaceRule {
            name: "http.url".to_string(),
            re: regex::Regex::new("(token/)([^/]*)").unwrap(),
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
            re: regex::Regex::new("(token/)([^/]*)").unwrap(),
            repl: "${1}?".to_string(),
            no_expansion: false,
        };
        let rule2 = replacer::ReplaceRule {
            name: "http.url".to_string(),
            re: regex::Regex::new("(broken/)([^/]*)").unwrap(),
            repl: "${1}?".to_string(),
            no_expansion: false,
        };
        assert_ne!(rule1, rule2);
    }
}
