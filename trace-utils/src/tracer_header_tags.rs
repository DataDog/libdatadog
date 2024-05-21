// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use hyper::http::HeaderValue;
use hyper::HeaderMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

macro_rules! parse_string_header {
    (
        $header_map:ident,
        { $($header_key:literal => $($field:ident).+ ,)+ }
    ) => {
        $(
            if let Some(header_value) = $header_map.get($header_key) {
                if let Ok(h) = header_value.to_str() {
                    $($field).+ = h;
                }
            }
        )+
    }
}

#[derive(Default, Debug, Serialize, Deserialize)]
pub struct TracerHeaderTags<'a> {
    pub lang: &'a str,
    pub lang_version: &'a str,
    pub lang_interpreter: &'a str,
    pub lang_vendor: &'a str,
    pub tracer_version: &'a str,
    pub container_id: &'a str,
    // specifies that the client has marked top-level spans, when set. Any non-empty value will
    // mean 'yes'.
    pub client_computed_top_level: bool,
    // specifies whether the client has computed stats so that the agent doesn't have to. Any
    // non-empty value will mean 'yes'.
    pub client_computed_stats: bool,
}

impl<'a> From<TracerHeaderTags<'a>> for HashMap<&'static str, String> {
    fn from(tags: TracerHeaderTags<'a>) -> HashMap<&'static str, String> {
        let mut headers = HashMap::from([
            ("datadog-meta-lang", tags.lang.to_string()),
            ("datadog-meta-lang-version", tags.lang_version.to_string()),
            (
                "datadog-meta-lang-interpreter",
                tags.lang_interpreter.to_string(),
            ),
            ("datadog-meta-lang-vendor", tags.lang_vendor.to_string()),
            (
                "datadog-meta-tracer-version",
                tags.tracer_version.to_string(),
            ),
            ("datadog-container-id", tags.container_id.to_string()),
        ]);
        headers.retain(|_, v| !v.is_empty());
        headers
    }
}

impl<'a> From<&'a HeaderMap<HeaderValue>> for TracerHeaderTags<'a> {
    fn from(headers: &'a HeaderMap<HeaderValue>) -> Self {
        let mut tags = TracerHeaderTags::default();
        parse_string_header!(
            headers,
            {
                "datadog-meta-lang" => tags.lang,
                "datadog-meta-lang-version" => tags.lang_version,
                "datadog-meta-lang-interpreter" => tags.lang_interpreter,
                "datadog-meta-lang-vendor" => tags.lang_vendor,
                "datadog-meta-tracer-version" => tags.tracer_version,
                "datadog-container-id" => tags.container_id,
            }
        );
        if headers.get("datadog-client-computed-top-level").is_some() {
            tags.client_computed_top_level = true;
        }
        if headers.get("datadog-client-computed-stats").is_some() {
            tags.client_computed_stats = true;
        }
        tags
    }
}
