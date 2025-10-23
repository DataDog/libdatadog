// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_trace_protobuf::pb;

use crate::{
    http::obfuscate_url_string,
    memcached::obfuscate_memcached_string,
    obfuscation_config::ObfuscationConfig,
    redis::{obfuscate_redis_string, remove_all_redis_args},
    replacer::replace_span_tags,
};

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
        "redis" => {
            if !config.obfuscation_redis_enabled || span.meta.is_empty() {
                return;
            }
            if let Some(redis_cmd) = span.meta.get_mut("redis.raw_command") {
                if config.obfuscation_redis_remove_all_args {
                    *redis_cmd = remove_all_redis_args(redis_cmd)
                }
                *redis_cmd = obfuscate_redis_string(redis_cmd)
            }
        }
        _ => {}
    }
    if let Some(tag_replace_rules) = &config.tag_replace_rules {
        replace_span_tags(span, tag_replace_rules, &mut String::new());
    }
}

#[cfg(test)]
mod tests {
    use datadog_trace_utils::test_utils;

    use crate::{obfuscation_config, replacer};

    use super::obfuscate_span;

    #[test]
    fn test_obfuscates_span_url_strings() {
        let mut span = test_utils::create_test_span(111, 222, 0, 1, true);
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
            obfuscation_redis_enabled: false,
            obfuscation_redis_remove_all_args: false,
        };
        obfuscate_span(&mut span, &obf_config);
        assert_eq!(
            span.meta.get("http.url").unwrap(),
            "http://foo.com/id/?/page/q?"
        )
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_replace_span_tags() {
        let mut span = test_utils::create_test_span(111, 222, 0, 1, true);
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
            obfuscation_redis_enabled: false,
            obfuscation_redis_remove_all_args: false,
        };

        obfuscate_span(&mut span, &obf_config);

        assert_eq!(span.meta.get("custom.tag").unwrap(), "/foo/bar/extra");
    }

    #[test]
    fn obfuscate_all_redis_args() {
        let mut span = test_utils::create_test_span(111, 222, 0, 1, true);
        span.r#type = "redis".to_string();
        span.meta.insert(
            "redis.raw_command".to_string(),
            "GEOADD key longitude latitude member".to_string(),
        );
        let obf_config = obfuscation_config::ObfuscationConfig {
            tag_replace_rules: None,
            http_remove_query_string: false,
            http_remove_path_digits: false,
            obfuscation_redis_enabled: true,
            obfuscation_redis_remove_all_args: true,
            obfuscate_memcached: false,
        };
        obfuscate_span(&mut span, &obf_config);
        assert_eq!(span.meta.get("redis.raw_command").unwrap(), "GEOADD ?")
    }

    #[test]
    fn obfuscate_redis_raw_query() {
        let mut span = test_utils::create_test_span(111, 222, 0, 1, true);
        span.r#type = "redis".to_string();
        span.meta.insert(
            "redis.raw_command".to_string(),
            "GEOADD key longitude latitude member".to_string(),
        );
        let obf_config = obfuscation_config::ObfuscationConfig {
            tag_replace_rules: None,
            http_remove_query_string: false,
            http_remove_path_digits: false,
            obfuscation_redis_enabled: true,
            obfuscation_redis_remove_all_args: false,
            obfuscate_memcached: false,
        };
        obfuscate_span(&mut span, &obf_config);
        assert_eq!(
            span.meta.get("redis.raw_command").unwrap(),
            "GEOADD key longitude latitude ?"
        )
    }
}
