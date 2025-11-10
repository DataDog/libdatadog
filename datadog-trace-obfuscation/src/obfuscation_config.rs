// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use log::{debug, error};
use std::env;

use libdd_common::config::parse_env;

use crate::replacer::{self, ReplaceRule};

#[derive(Debug)]
pub struct ObfuscationConfig {
    pub tag_replace_rules: Option<Vec<ReplaceRule>>,
    pub http_remove_query_string: bool,
    pub http_remove_path_digits: bool,
    pub obfuscate_memcached: bool,
    pub obfuscation_redis_enabled: bool,
    pub obfuscation_redis_remove_all_args: bool,
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
        let obfuscation_redis_enabled =
            parse_env::bool("DD_APM_OBFUSCATION_REDIS_ENABLED").unwrap_or(false);
        let obfuscation_redis_remove_all_args =
            parse_env::bool("DD_APM_OBFUSCATION_REDIS_REMOVE_ALL_ARGS").unwrap_or(false);

        let obfuscate_memcached =
            parse_env::bool("DD_APM_OBFUSCATION_MEMCACHED_ENABLED").unwrap_or(false);

        Ok(ObfuscationConfig {
            tag_replace_rules,
            http_remove_query_string,
            http_remove_path_digits,
            obfuscate_memcached,
            obfuscation_redis_enabled,
            obfuscation_redis_remove_all_args,
        })
    }
}
