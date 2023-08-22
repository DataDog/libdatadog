// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use datadog_trace_protobuf::pb;

use crate::{
    http::obfuscate_url_string, obfuscation_config::ObfuscationConfig, replacer::replace_span_tags,
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
        _ => {}
    }
    if let Some(tag_replace_rules) = &config.tag_replace_rules {
        replace_span_tags(span, tag_replace_rules)
    }
}

#[cfg(test)]
mod tests {
    use datadog_trace_protobuf::pb::Span;

    #[test]
    fn obfuscates_span_url_strings() {}

    #[test]
    fn replace_span_tags() {}
}
