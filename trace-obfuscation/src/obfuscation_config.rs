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
    pub sql_replace_digits: bool,
    pub sql_literal_escapes: bool,
}

impl ObfuscationConfig {
    pub fn new() -> Result<ObfuscationConfig, Box<dyn std::error::Error>> {
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
        let http_remove_query_string =
            parse_env::bool("DD_APM_OBFUSCATION_HTTP_REMOVE_QUERY_STRING").unwrap_or(false);
        let http_remove_path_digits =
            parse_env::bool("DD_APM_OBFUSCATION_HTTP_REMOVE_PATHS_WITH_DIGITS").unwrap_or(false);

        let obfuscate_memcached =
            parse_env::bool("DD_APM_OBFUSCATION_MEMCACHED_ENABLED").unwrap_or(false);

        let sql_replace_digits =
            parse_env::bool("DD_APM_OBFUSCATION_SQL_REPLACE_DIGITS").unwrap_or(false);

        let sql_literal_escapes =
            parse_env::bool("DD_APM_OBFUSCATION_SQL_LITERAL_ESCAPES").unwrap_or(false);

        Ok(ObfuscationConfig {
            tag_replace_rules,
            http_remove_query_string,
            http_remove_path_digits,
            obfuscate_memcached,
            sql_replace_digits,
            sql_literal_escapes,
        })
    }
}
