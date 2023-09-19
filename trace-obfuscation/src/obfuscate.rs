// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use datadog_trace_protobuf::pb;
use log::debug;

use crate::{
    http::obfuscate_url_string, memcached::obfuscate_memcached_string,
    obfuscation_config::ObfuscationConfig, replacer::replace_span_tags, sql::obfuscate_sql_string,
};

const TEXT_NON_PARSABLE: &str = "Non-parsable SQL query";

pub fn obfuscate_span(span: &mut pb::Span, config: &ObfuscationConfig) {
    match span.r#type.as_str() {
        "web" | "http" => {
            if span.meta.is_empty() {
                return;
            }
            if let Some(url) = span.meta.get_mut("http.url") {
                *url = obfuscate_url_string(
                    url,
                    config.http_remove_query_string,
                    config.http_remove_path_digits,
                )
            }
        }
        "memcached" if config.obfuscate_memcached => {
            if let Some(cmd) = span.meta.get_mut("memcached.command") {
                *cmd = obfuscate_memcached_string(cmd)
            }
        }
        "sql" | "cassandra" => {
            if span.resource.is_empty() {
                return;
            }
            let sql_obfuscation_result = obfuscate_sql_string(&span.resource, false);
            if let Some(err) = sql_obfuscation_result.error {
                debug!(
                    "Error parsing SQL query: {}. Resource: {}",
                    err, span.resource
                );
                span.resource = TEXT_NON_PARSABLE.to_string();
                span.meta
                    .insert("sql.query".to_string(), TEXT_NON_PARSABLE.to_string());
            }
            let query = sql_obfuscation_result.obfuscated_string.unwrap_or_default();
            span.resource = query.clone();
            span.meta.insert("sql.query".to_string(), query);
        }
        _ => {}
    }
    if let Some(tag_replace_rules) = &config.tag_replace_rules {
        replace_span_tags(span, tag_replace_rules)
    }
}

#[cfg(test)]
mod tests {
    use datadog_trace_utils::trace_test_utils;

    use crate::{obfuscation_config, replacer};

    use super::obfuscate_span;

    #[test]
    fn test_obfuscates_span_url_strings() {
        let mut span = trace_test_utils::create_test_span(111, 222, 0, 1, true);
        span.r#type = "http".to_string();
        span.meta.insert(
            "http.url".to_string(),
            "http://foo.com/id/123/page/q?search=bar&page=2".to_string(),
        );
        let obf_config = obfuscation_config::ObfuscationConfig {
            tag_replace_rules: None,
            http_remove_query_string: true,
            http_remove_path_digits: true,
            obfuscate_memcached: false,
            sql_replace_digits: false,
            sql_literal_escapes: false,
        };
        obfuscate_span(&mut span, &obf_config);
        assert_eq!(
            span.meta.get("http.url").unwrap(),
            "http://foo.com/id/?/page/q?"
        )
    }

    #[test]
    fn test_replace_span_tags() {
        let mut span = trace_test_utils::create_test_span(111, 222, 0, 1, true);
        span.meta
            .insert("custom.tag".to_string(), "/foo/bar/foo".to_string());

        let parsed_rules = replacer::parse_rules_from_string(
            r#"[{"name": "custom.tag", "pattern": "(/foo/bar/).*", "repl": "${1}extra"}]"#,
        )
        .unwrap();
        let obf_config = obfuscation_config::ObfuscationConfig {
            tag_replace_rules: Some(parsed_rules),
            http_remove_query_string: false,
            http_remove_path_digits: false,
            obfuscate_memcached: false,
            sql_replace_digits: false,
            sql_literal_escapes: false,
        };

        obfuscate_span(&mut span, &obf_config);

        assert_eq!(span.meta.get("custom.tag").unwrap(), "/foo/bar/extra");
    }

    #[test]
    fn test_obfuscate_span_sql_query() {
        let mut span = trace_test_utils::create_test_span(111, 222, 0, 1, true);
        span.resource = "SELECT username AS name from users where id = 2".to_string();
        span.r#type = "sql".to_string();
        let obf_config = obfuscation_config::ObfuscationConfig {
            tag_replace_rules: None,
            http_remove_query_string: false,
            http_remove_path_digits: false,
            obfuscate_memcached: false,
        };
        obfuscate_span(&mut span, &obf_config);
        assert_eq!(span.resource, "SELECT username from users where id = ?");
        assert_eq!(
            span.meta.get("sql.query").unwrap(),
            "SELECT username from users where id = ?"
        )
    }
}
