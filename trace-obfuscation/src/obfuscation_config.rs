// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use log::{debug, error};
use std::env;

use ddcommon::config::parse_env;

use crate::replacer::{self, ReplaceRule};

#[derive(Debug)]
pub struct ObfuscationConfig {
    pub tag_replace_rules: Option<Vec<ReplaceRule>>,
    pub http_remove_query_string: bool,
    pub http_remove_path_digits: bool,
    pub obfuscate_memcached: bool,
    pub obfuscate_sql: bool,
    pub sql_replace_digits: bool,
    pub sql_literal_escapes: bool,
    pub obfuscate_elasticsearch: bool,
    pub elasticsearch_keep_values: Option<Vec<String>>,
    pub elasticsearch_obfuscate_sql_values: Option<Vec<String>>,
    pub obfuscate_mongodb: bool,
    pub mongodb_keep_values: Option<Vec<String>>,
    pub mongodb_obfuscate_sql_values: Option<Vec<String>>,
}

impl ObfuscationConfig {
    pub fn new() -> Result<ObfuscationConfig, Box<dyn std::error::Error>> {
        // Tag Replacement
        let tag_replace_rules: Option<Vec<ReplaceRule>> = match env::var("DD_APM_REPLACE_TAGS") {
            Ok(replace_rules_str) => match replacer::parse_rules_from_string(&replace_rules_str) {
                Ok(res) => {
                    debug!("Successfully parsed DD_APM_REPLACE_TAGS: {res:?}");
                    Some(res)
                }
                Err(e) => {
                    error!("Failed to parse DD_APM_REPLACE_TAGS: {e}");
                    None
                }
            },
            Err(_) => None,
        };

        // HTTP
        let http_remove_query_string =
            parse_env::bool("DD_APM_OBFUSCATION_HTTP_REMOVE_QUERY_STRING").unwrap_or(false);
        let http_remove_path_digits =
            parse_env::bool("DD_APM_OBFUSCATION_HTTP_REMOVE_PATHS_WITH_DIGITS").unwrap_or(false);

        let obfuscate_memcached =
            parse_env::bool("DD_APM_OBFUSCATION_MEMCACHED_ENABLED").unwrap_or(false);

        let obfuscate_sql = parse_env::bool("DD_APM_OBFUSCATION_SQL_ENABLED").unwrap_or(true);

        // Elastic Search
        let obfuscate_elasticsearch =
            parse_env::bool("DD_APM_OBFUSCATION_ELASTICSEARCH_ENABLED").unwrap_or(false);
        let elasticsearch_keep_values_raw =
            parse_env::str_not_empty("DD_APM_OBFUSCATION_ELASTICSEARCH_KEEP_VALUES");
        let elasticsearch_keep_values = parse_arr_from_string(elasticsearch_keep_values_raw);

        let elasticsearch_obfuscate_sql_values_raw =
            parse_env::str_not_empty("DD_APM_OBFUSCATION_ELASTICSEARCH_OBFUSCATE_SQL_VALUES");
        let elasticsearch_obfuscate_sql_values =
            parse_arr_from_string(elasticsearch_obfuscate_sql_values_raw);

        // MongoDB
        let obfuscate_mongodb =
            parse_env::bool("DD_APM_OBFUSCATION_MONGODB_ENABLED").unwrap_or(false);
        let mongodb_keep_values_raw =
            parse_env::str_not_empty("DD_APM_OBFUSCATION_MONGODB_KEEP_VALUES");
        let mongodb_keep_values = parse_arr_from_string(mongodb_keep_values_raw);

        let mongodb_obfuscate_sql_values_raw =
            parse_env::str_not_empty("DD_APM_OBFUSCATION_MONGODB_OBFUSCATE_SQL_VALUES");
        let mongodb_obfuscate_sql_values = parse_arr_from_string(mongodb_obfuscate_sql_values_raw);

        Ok(ObfuscationConfig {
            tag_replace_rules,
            http_remove_query_string,
            http_remove_path_digits,
            obfuscate_memcached,
            obfuscate_sql,
            sql_replace_digits: false,
            sql_literal_escapes: false,
            obfuscate_elasticsearch,
            elasticsearch_keep_values,
            elasticsearch_obfuscate_sql_values,
            obfuscate_mongodb,
            mongodb_keep_values,
            mongodb_obfuscate_sql_values,
        })
    }

    /// Returns a new obfuscation config with all values false, for testing.
    pub fn new_test_config() -> ObfuscationConfig {
        ObfuscationConfig {
            tag_replace_rules: None,
            http_remove_query_string: false,
            http_remove_path_digits: false,
            obfuscate_memcached: false,
            obfuscate_sql: false,
            sql_replace_digits: false,
            sql_literal_escapes: false,
            obfuscate_elasticsearch: false,
            elasticsearch_keep_values: None,
            elasticsearch_obfuscate_sql_values: None,
            obfuscate_mongodb: false,
            mongodb_keep_values: None,
            mongodb_obfuscate_sql_values: None,
        }
    }
}

/// parses an Option<Vec<String>> from an Option<String>,
/// where the input string is a comma separated list
/// ex: "[a, b]"" or "a, b"
fn parse_arr_from_string(str: Option<String>) -> Option<Vec<String>> {
    str.as_ref()?;
    let mut s: String = str.unwrap().replace(',', " ");
    if s.starts_with('[') && s.ends_with(']') {
        s.remove(0);
        s.remove(s.len() - 1);
    }
    let res: Vec<&str> = s.split_ascii_whitespace().collect();
    if res.is_empty() {
        return None;
    }
    return Some(res.iter().map(|s| s.to_string()).collect());
}

#[cfg(test)]
mod tests {
    use duplicate::duplicate_item;

    use super::parse_arr_from_string;

    #[duplicate_item(
        test_name                           input                       expected;
        [test_parse_arr_from_string_1]  [None]                      [None];
        [test_parse_arr_from_string_2]  [Some("".to_string())]          [None];
        [test_parse_arr_from_string_3]  [Some("[]".to_string())]        [None];
        [test_parse_arr_from_string_4]  [Some("[a]".to_string())]       [Some(vec!["a".to_string()])];
        [test_parse_arr_from_string_5]  [Some("a".to_string())]         [Some(vec!["a".to_string()])];
        [test_parse_arr_from_string_6]  [Some("[a,b]".to_string())]     [Some(vec!["a".to_string(), "b".to_string()])];
        [test_parse_arr_from_string_7]  [Some("[a, b]".to_string())]    [Some(vec!["a".to_string(), "b".to_string()])];
        [test_parse_arr_from_string_8]  [Some("[a,  b]".to_string())]   [Some(vec!["a".to_string(), "b".to_string()])];
        [test_parse_arr_from_string_9]  [Some("a,b".to_string())]       [Some(vec!["a".to_string(), "b".to_string()])];
        [test_parse_arr_from_string_10] [Some("a,   b".to_string())]    [Some(vec!["a".to_string(), "b".to_string()])];
        [test_parse_arr_from_string_11] [Some("[a,b".to_string())]      [Some(vec!["[a".to_string(), "b".to_string()])];
        [test_parse_arr_from_string_12] [Some("a,b]".to_string())]      [Some(vec!["a".to_string(), "b]".to_string()])];
        [test_parse_arr_from_string_13] [Some("a],[b".to_string())]     [Some(vec!["a]".to_string(), "[b".to_string()])];
        [test_parse_arr_from_string_14] [Some("a,[],b".to_string())]    [Some(vec!["a".to_string(), "[]".to_string(), "b".to_string()])];
        [test_parse_arr_from_string_15] [Some("[,]".to_string())]       [None];
    )]
    #[test]
    fn test_name() {
        let result = parse_arr_from_string(input);
        assert_eq!(result, expected)
    }
}
