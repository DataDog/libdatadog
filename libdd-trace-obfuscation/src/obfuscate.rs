// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_trace_protobuf::pb::{self, attribute_array_value::AttributeArrayValueType};

use crate::{
    credit_cards::is_card_number,
    http::obfuscate_url_string,
    json::JsonObfuscator,
    memcached::obfuscate_memcached_string,
    obfuscation_config::ObfuscationConfig,
    redis::{obfuscate_redis_string, quantize_redis_string, remove_all_redis_args},
    replacer::replace_span_tags,
    sql::DbmsKind,
};

/// TAG_REDIS_RAW_COMMAND represents a redis raw command tag
const TAG_REDIS_RAW_COMMAND: &str = "redis.raw_command";
/// TAG_VALKEY_RAW_COMMAND represents a redis raw command tag
const TAG_VALKEY_RAW_COMMAND: &str = "valkey.raw_command";
/// TAG_MEMCACHED_COMMAND represents a memcached command tag
const TAG_MEMCACHED_COMMAND: &str = "memcached.command";
/// TAG_MONGO_DBQUERY represents a MongoDB query tag
const TAG_MONGO_DBQUERY: &str = "mongodb.query";
/// TAG_ELASTIC_BODY represents an Elasticsearch body tag
const TAG_ELASTIC_BODY: &str = "elasticsearch.body";
/// TAG_OPEN_SEARCH_BODY represents an OpenSearch body tag
const TAG_OPEN_SEARCH_BODY: &str = "opensearch.body";
/// TAG_SQLQUERY represents a SQL query tag
const TAG_SQLQUERY: &str = "sql.query";
/// TAG_HTTPURL represents an HTTP URL tag
const TAG_HTTPURL: &str = "http.url";
/// TAG_DBMS represents a DBMS tag
const TAG_DBMS: &str = "db.type";
/// TAG_CARD_NUMBER represents a card number tag
const TAG_CARD_NUMBER: &str = "card.number";

/// `obfuscate_span` goes through `span` fields and applies obfuscation on it
// TODO: return parsing errors in a vec
pub fn obfuscate_span(span: &mut pb::Span, config: &ObfuscationConfig) {
    for span_event in span.span_events.iter_mut() {
        obfuscate_span_event(span_event, config)
    }

    if let Some(credit_card) = span.meta.get_mut(TAG_CARD_NUMBER) {
        if is_card_number(&credit_card, config.credit_card.luhn) {
            *credit_card = "?".to_string();
        }
    }
    match span.r#type.as_str() {
        "web" | "http" if !span.meta.is_empty() => {
            if let Some(url) = span.meta.get_mut(TAG_HTTPURL) {
                *url = obfuscate_url_string(
                    url,
                    config.http_remove_query_string,
                    config.http_remove_paths_with_digits,
                )
            }
        }
        "memcached" if config.memcached.enabled => {
            if let Some(cmd) = span.meta.get_mut(TAG_MEMCACHED_COMMAND) {
                if config.memcached.keep_command {
                    *cmd = obfuscate_memcached_string(cmd)
                } else {
                    *cmd = "".to_string()
                }
            }
        }
        "redis" => {
            span.resource = quantize_redis_string(&span.resource);
            if !config.redis.enabled || span.meta.is_empty() {
                return;
            }
            if let Some(redis_cmd) = span.meta.get_mut(TAG_REDIS_RAW_COMMAND) {
                if config.redis.remove_all_args {
                    *redis_cmd = remove_all_redis_args(redis_cmd)
                }
                *redis_cmd = obfuscate_redis_string(redis_cmd)
            }
        }
        "valkey" => {
            span.resource = quantize_redis_string(&span.resource);
            if !config.valkey.enabled || span.meta.is_empty() {
                return;
            }
            if let Some(valkey_cmd) = span.meta.get_mut(TAG_VALKEY_RAW_COMMAND) {
                if config.valkey.remove_all_args {
                    *valkey_cmd = remove_all_redis_args(valkey_cmd)
                }
                *valkey_cmd = obfuscate_redis_string(valkey_cmd)
            }
        }
        "sql" | "cassandra" if !span.resource.is_empty() => {
            let dbms: DbmsKind = span
                .meta
                .get(TAG_DBMS)
                .map(String::as_str)
                .and_then(|dbms| TryInto::try_into(dbms).ok())
                .unwrap_or_default();
            let obfuscated_query = crate::sql::obfuscate_sql(&span.resource, &config.sql, dbms);
            span.resource = obfuscated_query.clone();
            span.meta.insert(TAG_SQLQUERY.to_owned(), obfuscated_query);
        }
        "elasticsearch" if config.elasticsearch.enabled => {
            if let Some(elastic_query) = span.meta.get_mut(TAG_ELASTIC_BODY) {
                // FIXME: optimization opportunity here: keep the obfuscators cached to avoid having
                // clones and re-hashsing strings when putting them in HashSets
                let (res, _err) =
                    JsonObfuscator::new(config.elasticsearch.clone()).obfuscate(elastic_query);
                *elastic_query = res;
            }
        }
        "opensearch" if config.opensearch.enabled => {
            if let Some(opensearch_query) = span.meta.get_mut(TAG_OPEN_SEARCH_BODY) {
                // FIXME: optimization opportunity here: keep the obfuscators cached to avoid having
                // clones and re-hashsing strings when putting them in HashSets
                let (res, _err) =
                    JsonObfuscator::new(config.opensearch.clone()).obfuscate(opensearch_query);
                *opensearch_query = res;
            }
        }
        "mongodb" if config.mongodb.enabled => {
            if let Some(mongodb_query) = span.meta.get_mut(TAG_MONGO_DBQUERY) {
                // FIXME: optimization opportunity here: keep the obfuscators cached to avoid having
                // clones and re-hashsing strings when putting them in HashSets
                let (res, _err) =
                    JsonObfuscator::new(config.mongodb.clone()).obfuscate(mongodb_query);

                *mongodb_query = res;
            }
        }

        _ => {}
    }
    if let Some(tag_replace_rules) = &config.tag_replace_rules {
        replace_span_tags(span, tag_replace_rules, &mut String::new());
    }
}

pub fn obfuscate_span_event(event: &mut pb::SpanEvent, config: &ObfuscationConfig) {
    if config.credit_card.enabled {
        for (k, v) in event.attributes.iter_mut() {
            if !should_obfuscate_cc_key(k, config) {
                continue;
            }
            let str_value = match v.r#type() {
                pb::attribute_any_value::AttributeAnyValueType::StringValue => {
                    v.string_value.to_string()
                }
                pb::attribute_any_value::AttributeAnyValueType::BoolValue => continue, /* Booleans can't be credit cards */
                pb::attribute_any_value::AttributeAnyValueType::IntValue => v.int_value.to_string(),
                pb::attribute_any_value::AttributeAnyValueType::DoubleValue => {
                    v.double_value.to_string()
                }
                pb::attribute_any_value::AttributeAnyValueType::ArrayValue => {
                    if let Some(array_value) = v.array_value.as_mut() {
                        obfuscate_attribute_array(array_value, config);
                    }
                    continue;
                }
            };
            if is_card_number(&str_value, config.credit_card.luhn) {
                v.string_value = "?".to_string();
            }
        }
    }
}

fn obfuscate_attribute_array(v: &mut pb::AttributeArray, config: &ObfuscationConfig) {
    for elt in v.values.iter_mut() {
        let string_value = match elt.r#type() {
            pb::attribute_array_value::AttributeArrayValueType::StringValue => {
                elt.string_value.clone()
            }
            pb::attribute_array_value::AttributeArrayValueType::BoolValue => continue, /* Booleans can't be credit cards */
            pb::attribute_array_value::AttributeArrayValueType::IntValue => {
                elt.int_value.to_string()
            }
            pb::attribute_array_value::AttributeArrayValueType::DoubleValue => {
                elt.double_value.to_string()
            }
        };
        let new_value = if is_card_number(&string_value, config.credit_card.luhn) {
            "?".to_string()
        } else {
            string_value.clone()
        };
        if new_value != string_value {
            *elt = pb::AttributeArrayValue {
                r#type: AttributeArrayValueType::StringValue.into(),
                string_value: new_value,
                bool_value: false,
                int_value: 0,
                double_value: 0.0,
            }
        }
    }
}

/// should_obfuscate_cc_key returns true if the value for the given key should be obfuscated
/// This is used to skip known safe attributes and specifically configured safe tags
fn should_obfuscate_cc_key(key: &str, config: &ObfuscationConfig) -> bool {
    match key {
	     | "_sample_rate"
		 | "_sampling_priority_v1"
		 | "account_id"
		 | "aws_account"
		 | "error"
		 | "error.msg"
		 | "error.type"
		 | "error.stack"
		 | "env"
		 | "graphql.field"
		 | "graphql.query"
		 | "graphql.type"
		 | "graphql.operation.name"
		 | "grpc.code"
		 | "grpc.method"
		 | "grpc.request"
		 | "http.status_code"
		 | "http.method"
		 | "runtime-id"
		 | "out.host"
		 | "out.port"
		 | "sampling.priority"
		 | "span.type"
		 | "span.name"
		 | "service.name"
		 | "service"
		 | "sql.query"
		 | "version"
		  // Data Job Monitoring tags - these values are frequently similar to credit card numbers
		 | "databricks_job_id"
		 | "databricks_job_run_id"
		 | "databricks_task_run_id"
		 | "config.spark_app_startTime"
		 | "config.spark_databricks_job_parentRunId" =>
		{return false;}
		_=> {}
	}
    if key.starts_with("_") {
        return false;
    }
    if config.credit_card.keep_values.contains(key) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use libdd_trace_utils::test_utils;

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
            http_remove_paths_with_digits: true,
            memcached: obfuscation_config::MemcachedConfig {
                enabled: false,
                keep_command: false,
            },
            credit_card: {
                obfuscation_config::CreditCardConfig {
                    enabled: false,
                    luhn: false,
                    keep_values: HashSet::new(),
                }
            },
            ..Default::default()
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
            http_remove_paths_with_digits: false,
            ..Default::default()
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
            redis: obfuscation_config::RedisConfig {
                enabled: true,
                remove_all_args: true,
            },
            ..Default::default()
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
            redis: obfuscation_config::RedisConfig {
                enabled: true,
                remove_all_args: false,
            },
            ..Default::default()
        };
        obfuscate_span(&mut span, &obf_config);
        assert_eq!(
            span.meta.get("redis.raw_command").unwrap(),
            "GEOADD key longitude latitude ?"
        )
    }
}
