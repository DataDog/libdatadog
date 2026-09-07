// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::borrow::Borrow;

use libdd_trace_protobuf::pb::{
    self, attribute_any_value::AttributeAnyValueType,
    attribute_array_value::AttributeArrayValueType,
};
use libdd_trace_utils::span::{
    v04::{self, AttributeAnyValue, AttributeArrayValue},
    SpanText, TraceData,
};

use crate::{
    credit_cards::{is_card_number, obfuscate_card_number},
    http::{obfuscate_url, obfuscate_url_string},
    memcached::{obfuscate_memcached, obfuscate_memcached_string},
    obfuscation_config::ObfuscationConfig,
    redis::{
        obfuscate_redis, obfuscate_redis_remove_all_args, obfuscate_redis_string, quantize_redis,
        quantize_redis_string, remove_all_redis_args,
    },
    replacer::{replace_span_tags, replace_span_tags_v04},
    sql::{obfuscate_sql_opt, DbmsKind, SqlObfuscationMode},
};

/// `TAG_REDIS_RAW_COMMAND` represents a redis raw command tag
const TAG_REDIS_RAW_COMMAND: &str = "redis.raw_command";
/// `TAG_VALKEY_RAW_COMMAND` represents a valkey raw command tag
const TAG_VALKEY_RAW_COMMAND: &str = "valkey.raw_command";
/// `TAG_MEMCACHED_COMMAND` represents a memcached command tag
const TAG_MEMCACHED_COMMAND: &str = "memcached.command";
/// `TAG_MONGO_DBQUERY` represents a `MongoDB` query tag
const TAG_MONGO_DBQUERY: &str = "mongodb.query";
/// `TAG_ELASTIC_BODY` represents an Elasticsearch body tag
const TAG_ELASTIC_BODY: &str = "elasticsearch.body";
/// `TAG_OPEN_SEARCH_BODY` represents an `OpenSearch` body tag
const TAG_OPEN_SEARCH_BODY: &str = "opensearch.body";
/// `TAG_SQLQUERY` represents a SQL query tag
const TAG_SQLQUERY: &str = "sql.query";
/// `TAG_HTTPURL` represents an HTTP URL tag
const TAG_HTTPURL: &str = "http.url";
/// `TAG_DBMS` represents a DBMS tag
const TAG_DBMS: &str = "db.type";
/// `TAG_CARD_NUMBER` represents a card number tag
const TAG_CARD_NUMBER: &str = "card.number";

/// Obfuscate a resource name for client-side stats (Version 1).
///
/// Applies the same resource transformations as `obfuscate_pb_span`, but only for span types whose
/// resource names are modified:
/// - `"sql"`, `"cassandra"`: SQL obfuscation
/// - `"redis"`, `"valkey"`: Redis quantization (command names only)
///
/// Returns `Some(obfuscated)` if the resource was modified, `None` if no obfuscation was needed.
#[must_use]
pub fn obfuscate_resource_for_stats(
    span_type: &str,
    resource: &str,
    dbms_hint: Option<&str>,
    sql_obfuscation_mode: SqlObfuscationMode,
) -> Option<String> {
    match span_type {
        "sql" | "cassandra" if !resource.is_empty() => {
            let dbms: DbmsKind = dbms_hint
                .and_then(|d| d.try_into().ok())
                .unwrap_or_default();
            let config = &crate::sql::SqlObfuscateConfig {
                obfuscation_mode: sql_obfuscation_mode,
                ..Default::default()
            };
            Some(crate::sql::obfuscate_sql(resource, config, dbms))
        }
        "redis" | "valkey" => Some(quantize_redis_string(resource)),
        _ => None,
    }
}

/// `obfuscate_pb_span` goes through `span` fields and applies obfuscation on it
// TODO(APMSP-2764): return parsing errors in a vec to log them ?
pub fn obfuscate_pb_span(span: &mut pb::Span, config: &ObfuscationConfig) {
    for span_event in &mut span.span_events {
        obfuscate_span_event(span_event, config);
    }

    if let Some(credit_card) = span.meta.get_mut(TAG_CARD_NUMBER) {
        if config.credit_cards.enabled && is_card_number(&credit_card, config.credit_cards.luhn) {
            *credit_card = "?".to_string();
        }
    }
    match span.r#type.as_str() {
        "web" | "http" if !span.meta.is_empty() => {
            if let Some(url) = span.meta.get_mut(TAG_HTTPURL) {
                *url = obfuscate_url_string(
                    url,
                    config.http.remove_query_string,
                    config.http.remove_paths_with_digits,
                );
            }
        }
        "memcached" if config.memcached.enabled => {
            if let Some(cmd) = span.meta.get_mut(TAG_MEMCACHED_COMMAND) {
                if config.memcached.keep_command {
                    *cmd = obfuscate_memcached_string(cmd);
                } else {
                    *cmd = String::new();
                }
            }
        }
        "redis" => {
            span.resource = quantize_redis_string(&span.resource);
            if config.redis.enabled && !span.meta.is_empty() {
                if let Some(redis_cmd) = span.meta.get_mut(TAG_REDIS_RAW_COMMAND) {
                    if config.redis.remove_all_args {
                        *redis_cmd = remove_all_redis_args(redis_cmd);
                    } else {
                        *redis_cmd = obfuscate_redis_string(redis_cmd);
                    }
                }
            }
        }
        "valkey" => {
            span.resource = quantize_redis_string(&span.resource);
            if config.valkey.enabled && !span.meta.is_empty() {
                if let Some(valkey_cmd) = span.meta.get_mut(TAG_VALKEY_RAW_COMMAND) {
                    if config.valkey.remove_all_args {
                        *valkey_cmd = remove_all_redis_args(valkey_cmd);
                    } else {
                        *valkey_cmd = obfuscate_redis_string(valkey_cmd);
                    }
                }
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
            span.resource.clone_from(&obfuscated_query);
            span.meta.insert(TAG_SQLQUERY.to_owned(), obfuscated_query);
        }
        "elasticsearch" if config.elasticsearch.config().enabled => {
            if let Some(elastic_query) = span.meta.get_mut(TAG_ELASTIC_BODY) {
                let (res, _err) = config.elasticsearch.obfuscate(elastic_query);
                *elastic_query = res;
            }
        }
        "opensearch" if config.opensearch.config().enabled => {
            if let Some(opensearch_query) = span.meta.get_mut(TAG_OPEN_SEARCH_BODY) {
                let (res, _err) = config.opensearch.obfuscate(opensearch_query);
                *opensearch_query = res;
            }
        }
        "mongodb" if config.mongodb.config().enabled => {
            if let Some(mongodb_query) = span.meta.get_mut(TAG_MONGO_DBQUERY) {
                let (res, _err) = config.mongodb.obfuscate(mongodb_query);

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
    if config.credit_cards.enabled {
        for (k, v) in &mut event.attributes {
            if !should_obfuscate_cc_key(k, config) {
                continue;
            }
            let str_value = match v.r#type() {
                pb::attribute_any_value::AttributeAnyValueType::StringValue => {
                    v.string_value.clone()
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
            if is_card_number(&str_value, config.credit_cards.luhn) {
                v.string_value = "?".to_string();
                v.r#type = AttributeAnyValueType::StringValue.into();
            }
        }
    }
}

fn obfuscate_attribute_array(v: &mut pb::AttributeArray, config: &ObfuscationConfig) {
    for elt in &mut v.values {
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
        if is_card_number(&string_value, config.credit_cards.luhn) {
            elt.string_value = "?".to_string();
            elt.r#type = AttributeArrayValueType::StringValue.into();
        }
    }
}

/// Borrows the `&str` view of a [`SpanText`] value unambiguously.
fn as_str<S: SpanText>(s: &S) -> &str {
    <S as Borrow<str>>::borrow(s)
}

/// Runs an obfuscator over a [`SpanText`] field and writes back through [`SpanText::from_owned`]
/// only on a hit. A `None` result leaves the field untouched, avoiding any allocation.
fn apply<S: SpanText>(field: &mut S, f: impl FnOnce(&str) -> Option<String>) {
    if let Some(new) = f(as_str(field)) {
        *field = S::from_owned(new);
    }
}

/// Obfuscates the fields of a [`v04::Span`].
///
/// Mirrors [`obfuscate_pb_span`] but targets the generic [`v04::Span`] whose string fields are the
/// immutable [`SpanText`] type. Each obfuscator has a cheap precheck, so unmodified fields don't
/// allocate.
// TODO(APMSP-2764): return parsing errors in a vec to log them ?
pub fn obfuscate_v04_span<T: TraceData>(span: &mut v04::Span<T>, config: &ObfuscationConfig) {
    for span_event in &mut span.span_events {
        obfuscate_v04_span_event(span_event, config);
    }

    if config.credit_cards.enabled {
        if let Some(credit_card) = span.meta.get_mut(TAG_CARD_NUMBER) {
            apply(credit_card, |v| {
                obfuscate_card_number(v, config.credit_cards.luhn)
            });
        }
    }

    match as_str(&span.r#type) {
        "web" | "http" if !span.meta.is_empty() => {
            if let Some(url) = span.meta.get_mut(TAG_HTTPURL) {
                apply(url, |u| {
                    obfuscate_url(
                        u,
                        config.http.remove_query_string,
                        config.http.remove_paths_with_digits,
                    )
                });
            }
        }
        "memcached" if config.memcached.enabled => {
            if let Some(cmd) = span.meta.get_mut(TAG_MEMCACHED_COMMAND) {
                apply(cmd, |c| {
                    obfuscate_memcached(c, config.memcached.keep_command)
                });
            }
        }
        "redis" => {
            apply(&mut span.resource, quantize_redis);
            if config.redis.enabled && !span.meta.is_empty() {
                if let Some(redis_cmd) = span.meta.get_mut(TAG_REDIS_RAW_COMMAND) {
                    if config.redis.remove_all_args {
                        apply(redis_cmd, obfuscate_redis_remove_all_args);
                    } else {
                        apply(redis_cmd, obfuscate_redis);
                    }
                }
            }
        }
        "valkey" => {
            apply(&mut span.resource, quantize_redis);
            if config.valkey.enabled && !span.meta.is_empty() {
                if let Some(valkey_cmd) = span.meta.get_mut(TAG_VALKEY_RAW_COMMAND) {
                    if config.valkey.remove_all_args {
                        apply(valkey_cmd, obfuscate_redis_remove_all_args);
                    } else {
                        apply(valkey_cmd, obfuscate_redis);
                    }
                }
            }
        }
        "sql" | "cassandra" if !span.resource.borrow().is_empty() => {
            let dbms: DbmsKind = span
                .meta
                .get(TAG_DBMS)
                .map(as_str)
                .and_then(|dbms| TryInto::try_into(dbms).ok())
                .unwrap_or_default();
            if let Some(query) = obfuscate_sql_opt(as_str(&span.resource), &config.sql, dbms) {
                span.resource = T::Text::from_owned(query.clone());
                span.meta.insert(
                    T::Text::from_static_str(TAG_SQLQUERY),
                    T::Text::from_owned(query),
                );
            }
        }
        "elasticsearch" if config.elasticsearch.config().enabled => {
            if let Some(elastic_query) = span.meta.get_mut(TAG_ELASTIC_BODY) {
                apply(elastic_query, |q| config.elasticsearch.obfuscate_opt(q));
            }
        }
        "opensearch" if config.opensearch.config().enabled => {
            if let Some(opensearch_query) = span.meta.get_mut(TAG_OPEN_SEARCH_BODY) {
                apply(opensearch_query, |q| config.opensearch.obfuscate_opt(q));
            }
        }
        "mongodb" if config.mongodb.config().enabled => {
            if let Some(mongodb_query) = span.meta.get_mut(TAG_MONGO_DBQUERY) {
                apply(mongodb_query, |q| config.mongodb.obfuscate_opt(q));
            }
        }
        _ => {}
    }

    if let Some(tag_replace_rules) = &config.tag_replace_rules {
        replace_span_tags_v04(span, tag_replace_rules);
    }
}

/// Obfuscates credit-card numbers inside the attributes of a [`v04::SpanEvent`].
fn obfuscate_v04_span_event<T: TraceData>(
    event: &mut v04::SpanEvent<T>,
    config: &ObfuscationConfig,
) {
    if !config.credit_cards.enabled {
        return;
    }
    for (k, v) in &mut event.attributes {
        if !should_obfuscate_cc_key(as_str(k), config) {
            continue;
        }
        match v {
            AttributeAnyValue::SingleValue(value) => {
                obfuscate_v04_attribute_value(value, config);
            }
            AttributeAnyValue::Array(values) => {
                for value in values.iter_mut() {
                    obfuscate_v04_attribute_value(value, config);
                }
            }
        }
    }
}

/// Obfuscates a single [`v04::AttributeArrayValue`] if it looks like a credit-card number.
///
/// Integers and doubles are stringified for the check. On a hit the variant becomes `String("?")`,
/// like [`obfuscate_attribute_array`] switching the pb value type to string.
fn obfuscate_v04_attribute_value<T: TraceData>(
    value: &mut AttributeArrayValue<T>,
    config: &ObfuscationConfig,
) {
    let is_card = match value {
        AttributeArrayValue::String(s) => is_card_number(as_str(s), config.credit_cards.luhn),
        AttributeArrayValue::Boolean(_) => false,
        AttributeArrayValue::Integer(i) => is_card_number(i.to_string(), config.credit_cards.luhn),
        AttributeArrayValue::Double(d) => is_card_number(d.to_string(), config.credit_cards.luhn),
    };
    if is_card {
        *value = AttributeArrayValue::String(T::Text::from_static_str("?"));
    }
}

/// `should_obfuscate_cc_key` returns true if the value for the given key should be obfuscated
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
    if key.starts_with('_') {
        return false;
    }
    if config.credit_cards.keep_values.contains(key) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{obfuscate_pb_span, obfuscate_resource_for_stats};
    use crate::{obfuscation_config, replacer};
    use libdd_trace_utils::test_utils;

    // test helper with default params
    fn obfuscate_stats(span_type: &str, resource: &str) -> Option<String> {
        obfuscate_resource_for_stats(
            span_type,
            resource,
            None,
            crate::sql::SqlObfuscationMode::default(),
        )
    }

    #[test]
    fn test_obfuscate_resource_for_stats_sql() {
        let result = obfuscate_stats("sql", "SELECT * FROM users WHERE id = 42");
        assert_eq!(result.unwrap(), "SELECT * FROM users WHERE id = ?");
    }

    #[test]
    fn test_obfuscate_resource_for_stats_cassandra() {
        let result = obfuscate_stats("cassandra", "SELECT * FROM table1 WHERE id = 42");
        assert_eq!(result.unwrap(), "SELECT * FROM table1 WHERE id = ?");
    }

    #[test]
    fn test_obfuscate_resource_for_stats_redis() {
        let result = obfuscate_stats("redis", "SET mykey myvalue\nGET mykey");
        assert!(result.is_some());
        // quantize_redis_string extracts command names
        assert_eq!(result.unwrap(), "SET GET");
    }

    #[test]
    fn test_obfuscate_resource_for_stats_valkey() {
        let result = obfuscate_stats("valkey", "SET mykey myvalue\nGET mykey");
        assert_eq!(result.unwrap(), "SET GET");
    }

    #[test]
    fn test_obfuscate_resource_for_stats_no_match() {
        assert!(obfuscate_stats("http", "/api/users").is_none());
        assert!(obfuscate_stats("web", "/api/users").is_none());
        assert!(obfuscate_stats("grpc", "MyService/MyMethod").is_none());
    }

    #[test]
    fn test_obfuscate_resource_for_stats_empty_sql() {
        assert!(obfuscate_stats("sql", "").is_none());
    }

    #[test]
    fn test_obfuscates_span_url_strings() {
        let mut span = test_utils::create_test_span(111, 222, 0, 1, true);
        span.r#type = "http".to_string();
        span.meta.insert(
            "http.url".to_string(),
            "http://foo.com/id/123/page/q?search=bar&page=2".to_string(),
        );
        let obf_config = obfuscation_config::ObfuscationConfig {
            http: obfuscation_config::HttpConfig {
                remove_query_string: true,
                remove_paths_with_digits: true,
            },
            ..Default::default()
        };
        obfuscate_pb_span(&mut span, &obf_config);
        assert_eq!(
            span.meta.get("http.url").unwrap(),
            "http://foo.com/id/?/page/q?"
        );
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
            ..Default::default()
        };

        obfuscate_pb_span(&mut span, &obf_config);

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
        obfuscate_pb_span(&mut span, &obf_config);
        assert_eq!(span.meta.get("redis.raw_command").unwrap(), "GEOADD ?");
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
        obfuscate_pb_span(&mut span, &obf_config);
        assert_eq!(
            span.meta.get("redis.raw_command").unwrap(),
            "GEOADD key longitude latitude ?"
        );
    }
}

#[cfg(test)]
mod v04_tests {
    use super::obfuscate_v04_span;
    use crate::obfuscation_config::{
        CreditCardConfig, HttpConfig, MemcachedConfig, ObfuscationConfig, RedisConfig,
    };
    use crate::replacer;
    use libdd_tinybytes::BytesString;
    use libdd_trace_utils::span::v04::{
        AttributeAnyValue, AttributeArrayValue, SpanBytes, SpanEventBytes,
    };
    use std::collections::HashMap;

    fn bs(s: &str) -> BytesString {
        BytesString::from_string(s.to_string())
    }

    fn test_span() -> SpanBytes {
        SpanBytes {
            service: bs("test-service"),
            name: bs("test_name"),
            resource: bs("test-resource"),
            ..Default::default()
        }
    }

    #[test]
    fn test_obfuscates_span_url_strings() {
        let mut span = test_span();
        span.r#type = bs("http");
        span.meta.insert(
            bs("http.url"),
            bs("http://foo.com/id/123/page/q?search=bar&page=2"),
        );
        let obf_config = ObfuscationConfig {
            http: HttpConfig {
                remove_query_string: true,
                remove_paths_with_digits: true,
            },
            ..Default::default()
        };
        obfuscate_v04_span(&mut span, &obf_config);
        assert_eq!(
            span.meta.get("http.url").unwrap().as_str(),
            "http://foo.com/id/?/page/q?"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_replace_span_tags() {
        let mut span = test_span();
        span.meta.insert(bs("custom.tag"), bs("/foo/bar/foo"));

        let parsed_rules = replacer::parse_rules_from_string(
            r#"[{"name": "custom.tag", "pattern": "(/foo/bar/).*", "repl": "${1}extra"}]"#,
        )
        .unwrap();
        let obf_config = ObfuscationConfig {
            tag_replace_rules: Some(parsed_rules),
            ..Default::default()
        };

        obfuscate_v04_span(&mut span, &obf_config);

        assert_eq!(
            span.meta.get("custom.tag").unwrap().as_str(),
            "/foo/bar/extra"
        );
    }

    #[test]
    fn obfuscate_all_redis_args() {
        let mut span = test_span();
        span.r#type = bs("redis");
        span.meta.insert(
            bs("redis.raw_command"),
            bs("GEOADD key longitude latitude member"),
        );
        let obf_config = ObfuscationConfig {
            redis: RedisConfig {
                enabled: true,
                remove_all_args: true,
            },
            ..Default::default()
        };
        obfuscate_v04_span(&mut span, &obf_config);
        assert_eq!(
            span.meta.get("redis.raw_command").unwrap().as_str(),
            "GEOADD ?"
        );
    }

    #[test]
    fn obfuscate_redis_raw_query() {
        let mut span = test_span();
        span.r#type = bs("redis");
        span.meta.insert(
            bs("redis.raw_command"),
            bs("GEOADD key longitude latitude member"),
        );
        let obf_config = ObfuscationConfig {
            redis: RedisConfig {
                enabled: true,
                remove_all_args: false,
            },
            ..Default::default()
        };
        obfuscate_v04_span(&mut span, &obf_config);
        assert_eq!(
            span.meta.get("redis.raw_command").unwrap().as_str(),
            "GEOADD key longitude latitude ?"
        );
    }

    #[test]
    fn obfuscate_sql_resource_and_query() {
        let mut span = test_span();
        span.r#type = bs("sql");
        span.resource = bs("SELECT * FROM users WHERE id = 42");
        obfuscate_v04_span(&mut span, &ObfuscationConfig::default());
        assert_eq!(span.resource.as_str(), "SELECT * FROM users WHERE id = ?");
        assert_eq!(
            span.meta.get("sql.query").unwrap().as_str(),
            "SELECT * FROM users WHERE id = ?"
        );
    }

    #[test]
    fn obfuscate_memcached_command() {
        let mut span = test_span();
        span.r#type = bs("memcached");
        span.meta
            .insert(bs("memcached.command"), bs("set mykey 0 60 5\r\nvalue"));
        let obf_config = ObfuscationConfig {
            memcached: MemcachedConfig {
                enabled: true,
                keep_command: true,
            },
            ..Default::default()
        };
        obfuscate_v04_span(&mut span, &obf_config);
        assert_eq!(
            span.meta.get("memcached.command").unwrap().as_str(),
            "set mykey 0 60 5"
        );
    }

    #[test]
    fn obfuscate_credit_card_meta() {
        let mut span = test_span();
        span.meta.insert(bs("card.number"), bs("4111111111111111"));
        let obf_config = ObfuscationConfig {
            credit_cards: CreditCardConfig {
                enabled: true,
                luhn: false,
                ..Default::default()
            },
            ..Default::default()
        };
        obfuscate_v04_span(&mut span, &obf_config);
        assert_eq!(span.meta.get("card.number").unwrap().as_str(), "?");
    }

    #[test]
    fn obfuscate_span_event_credit_cards() {
        let mut span = test_span();
        let mut attributes = HashMap::new();
        attributes.insert(
            bs("cc"),
            AttributeAnyValue::SingleValue(AttributeArrayValue::String(bs("4111111111111111"))),
        );
        attributes.insert(
            bs("cc_array"),
            AttributeAnyValue::Array(vec![AttributeArrayValue::String(bs("4111111111111111"))]),
        );
        span.span_events.push(SpanEventBytes {
            time_unix_nano: 0,
            name: bs("event"),
            attributes,
        });
        let obf_config = ObfuscationConfig {
            credit_cards: CreditCardConfig {
                enabled: true,
                luhn: false,
                ..Default::default()
            },
            ..Default::default()
        };
        obfuscate_v04_span(&mut span, &obf_config);
        let attrs = &span.span_events[0].attributes;
        assert!(matches!(
            attrs.get("cc"),
            Some(AttributeAnyValue::SingleValue(AttributeArrayValue::String(s))) if s.as_str() == "?"
        ));
        assert!(matches!(
            attrs.get("cc_array"),
            Some(AttributeAnyValue::Array(v)) if matches!(&v[0], AttributeArrayValue::String(s) if s.as_str() == "?")
        ));
    }
}
