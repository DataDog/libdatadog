// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use datadog_trace_protobuf::pb;
use regex::Regex;

#[derive(Debug)]
pub struct ReplaceRule<'a> {
    // name specifies the name of the tag that the replace rule addresses. However,
    // some exceptions apply such as:
    // * "resource.name" will target the resource
    // * "*" will target all tags and the resource
    name: &'a str,

    // re holds the regex pattern for matching.
    re: regex::Regex,

    // repl specifies the replacement string to be used when Pattern matches.
    repl: &'a str,
}

/// replace_trace_tags replaces the tag values of all spans within a trace with a given set of rules.
pub fn replace_trace_tags(trace: &mut [pb::Span], rules: &[ReplaceRule]) {
    for rule in rules {
        for span in trace.iter_mut() {
            match rule.name {
                "*" => {
                    for (_, val) in span.meta.iter_mut() {
                        *val = rule.re.replace_all(val, rule.repl).to_string();
                    }
                }
                "resource.name" => {
                    span.resource = rule.re.replace_all(&span.resource, rule.repl).to_string();
                }
                _ => {
                    if let Some(val) = span.meta.get_mut(rule.name) {
                        let replaced_tag = rule.re.replace_all(val, rule.repl).to_string();
                        *val = replaced_tag;
                    }
                }
            }
        }
    }
}

/// parse_rules_from_string takes an array of rules, represented as an array of length 3 arrays
/// holding the tag name, regex pattern, and replacement string as strings.
/// * returns a vec of ReplaceRules
pub fn parse_rules_from_string<'a>(
    rules: &'a [[&'a str; 3]],
) -> anyhow::Result<Vec<ReplaceRule<'a>>> {
    let mut vec: Vec<ReplaceRule> = Vec::with_capacity(rules.len());

    for [name, pattern, repl] in rules {
        let compiled_regex = match Regex::new(pattern) {
            Ok(res) => res,
            Err(err) => {
                anyhow::bail!("Obfuscator Error: Error while parsing rule: {}", err)
            }
        };
        vec.push(ReplaceRule {
            name,
            re: compiled_regex,
            repl,
        });
    }
    Ok(vec)
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
        rules       [&[
                        ["http.url", "(token/)([^/]*)", "${1}?"],
                        ["http.url", "guid", "[REDACTED]"],
                        ["custom.tag", "(/foo/bar/).*", "${1}extra"],
                    ]]
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
        rules       [&[
                        ["*", "(token/)([^/]*)", "${1}?"],
                        ["*", "this", "that"],
                        ["http.url", "guid", "[REDACTED]"],
                        ["custom.tag", "(/foo/bar/).*", "${1}extra"],
                        ["resource.name", "prod", "stage"],
                    ]]
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
        let result = replacer::parse_rules_from_string(&[["http.url", ")", "${1}?"]]);
        assert!(result.is_err());
    }
}
