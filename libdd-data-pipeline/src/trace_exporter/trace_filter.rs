// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! Trace-level filter logic for client-side stats (filter_tags, filter_tags_regex,
//! ignore_resources as published by the agent's /info endpoint).
use std::{borrow::Borrow as _, sync::Arc};

use arc_swap::ArcSwap;
use libdd_common::regex_engine::Regex;
use libdd_trace_stats::span_concentrator::StatSpan;
use libdd_trace_utils::span::trace_utils::get_root_span_index;
use tracing::{debug, error};

trait TagFilter {
    /// Returns true if the given tag value matches the Filterer.
    fn matches_tag_value(&self, value: &str) -> bool;
    // Getter to the key field
    fn key(&self) -> &str;
}

#[derive(Debug)]
struct TagLiteralFilter {
    key: String,
    value: Option<String>,
}

#[derive(Debug)]
struct TagRegexFilter {
    key: String,
    value: Option<Regex>,
}

/// Parsed config
#[derive(Debug, Default)]
struct TraceFiltererConf {
    reject: Vec<TagLiteralFilter>,
    reject_regex: Vec<TagRegexFilter>,

    require: Vec<TagLiteralFilter>,
    require_regex: Vec<TagRegexFilter>,

    ignore_resources: Vec<Regex>,
}

#[derive(Debug)]
pub struct TraceFilterer {
    conf: ArcSwap<TraceFiltererConf>,
}

impl TagFilter for TagLiteralFilter {
    fn matches_tag_value(&self, value: &str) -> bool {
        match &self.value {
            None => true, // No value requirement => Any value is a match
            Some(required_value) => value == required_value,
        }
    }

    fn key(&self) -> &str {
        &self.key
    }
}

impl TagFilter for TagRegexFilter {
    fn matches_tag_value(&self, value: &str) -> bool {
        match &self.value {
            None => true, // No value requirement => Any value is a match
            Some(pattern) => pattern.is_match(value),
        }
    }

    fn key(&self) -> &str {
        &self.key
    }
}

impl TraceFiltererConf {
    fn compile_literal_filters(filters: &[String]) -> Vec<TagLiteralFilter> {
        let mut tag_regex_filters = Vec::new();
        for filter in filters {
            let (key, value) = match filter.split_once(":") {
                Some((key, value)) if !value.trim().is_empty() => {
                    (key.trim(), Some(value.trim().to_owned()))
                }
                _ => (filter.trim(), None),
            };
            if key.is_empty() {
                error!(
                    ?filter,
                    "Invalid tag filter with empty key value, skipping it"
                );
                continue;
            }

            tag_regex_filters.push(TagLiteralFilter {
                key: key.to_owned(),
                value,
            });
        }

        tag_regex_filters
    }

    fn compile_regex_filters(filters: &[String]) -> Vec<TagRegexFilter> {
        let mut tag_regex_filters = Vec::new();
        for filter in filters {
            let (key, value) = match filter.split_once(":") {
                Some((key, value)) if !value.trim().is_empty() => (key.trim(), Some(value.trim())),
                _ => (filter.trim(), None),
            };
            if key.is_empty() {
                error!(
                    ?filter,
                    "Invalid tag filter with empty key value, skipping it"
                );
                continue;
            }

            let value = match value {
                Some(value) => match Regex::new(value) {
                    Ok(regex) => Some(regex),
                    Err(err) => {
                        error!(
                            ?filter,
                            ?err,
                            "Invalid regex pattern in tag filter's value, skipping it"
                        );
                        continue;
                    }
                },
                None => None,
            };

            tag_regex_filters.push(TagRegexFilter {
                key: key.to_owned(),
                value,
            });
        }

        tag_regex_filters
    }

    fn compile_resource_filters(ignore_resources: &[String]) -> Vec<Regex> {
        ignore_resources
            .iter()
            .filter_map(|regex| {
                Regex::new(regex)
                    .inspect_err(|err| {
                        error!(
                            ?regex,
                            ?err,
                            "Invalid regex pattern in ignore resources filter, skipping it"
                        )
                    })
                    .ok()
            })
            .collect()
    }

    fn parse(
        filter_tags: &crate::agent_info::schema::FilterTagsConfig,
        filter_tags_regex: &crate::agent_info::schema::FilterTagsConfig,
        ignore_resources: &[String],
    ) -> Self {
        let require_regex = Self::compile_regex_filters(&filter_tags_regex.require);
        let reject_regex = Self::compile_regex_filters(&filter_tags_regex.reject);
        let require = Self::compile_literal_filters(&filter_tags.require);
        let reject = Self::compile_literal_filters(&filter_tags.reject);
        let ignore_resources = Self::compile_resource_filters(ignore_resources);

        TraceFiltererConf {
            reject,
            require,
            reject_regex,
            require_regex,
            ignore_resources,
        }
    }
}

impl TraceFilterer {
    #[cfg(test)]
    fn new(
        filter_tags: &crate::agent_info::schema::FilterTagsConfig,
        filter_tags_regex: &crate::agent_info::schema::FilterTagsConfig,
        ignore_resources: &[String],
    ) -> Self {
        let conf = TraceFiltererConf::parse(filter_tags, filter_tags_regex, ignore_resources);
        Self {
            conf: ArcSwap::from_pointee(conf),
        }
    }
    pub fn with_empty_conf() -> Self {
        Self {
            conf: ArcSwap::from_pointee(TraceFiltererConf::default()),
        }
    }

    pub fn update_conf(
        &self,
        filter_tags: &crate::agent_info::schema::FilterTagsConfig,
        filter_tags_regex: &crate::agent_info::schema::FilterTagsConfig,
        ignore_resources: &[String],
    ) {
        let new_conf = TraceFiltererConf::parse(filter_tags, filter_tags_regex, ignore_resources);
        self.conf.swap(Arc::new(new_conf));
    }

    pub fn filter_traces<T: libdd_trace_utils::span::TraceData>(
        &self,
        traces: &mut Vec<Vec<libdd_trace_utils::span::v04::Span<T>>>,
    ) {
        let conf = self.conf.load();
        traces.retain(|trace| {
            let Ok(root_span_index) = get_root_span_index(trace) else {
                return true;
            };
            let root_span = &trace[root_span_index];
            let should_drop = Self::should_drop(&conf, root_span);
            if should_drop {
                debug!("Trace rejected as it fails to meet tag requirements. root: %v");
            }
            !should_drop
        });
    }

    /// Checks if the trace with root span `root_span` should be dropped based on filter
    /// configuration.
    ///
    /// Applies a subset of trace normalization logic from `libdd-trace-normalization` before
    /// checking.
    // 1. Resource filtering: If the root span's resource name matches any pattern in
    //    ignore_resources, reject the trace.
    // 2. Reject filtering: If any tag on the root span matches filters in filter_tags.reject or
    //    filter_tags_regex.reject, reject the trace.
    // 3. Require filtering: If filter_tags.require or filter_tags_regex.require contain any
    //    filters, all of them must match tags on the root span. If any required filter doesn't
    //    match, reject the trace.
    fn should_drop<T: libdd_trace_utils::span::TraceData>(
        conf: &TraceFiltererConf,
        root_span: &libdd_trace_utils::span::v04::Span<T>,
    ) -> bool {
        if !conf.ignore_resources.is_empty() {
            let span_resource = root_span.resource();
            // Normalization
            let span_resource = if span_resource.is_empty() {
                let span_name = root_span.name();
                debug!(
                    ?span_name,
                    "Trace filter fixing malformed trace. Resource is empty so using name instead"
                );
                span_name
            } else {
                span_resource
            };

            if conf
                .ignore_resources
                .iter()
                .any(|resource_pattern| resource_pattern.is_match(span_resource))
            {
                return true;
            }
        }

        if conf
            .reject
            .iter()
            .any(|filter| Self::check_tag_filter_with_normalization(filter, root_span))
        {
            return true;
        }

        if conf
            .reject_regex
            .iter()
            .any(|filter| Self::check_tag_filter_with_normalization(filter, root_span))
        {
            return true;
        }

        if !conf
            .require
            .iter()
            .all(|filter| Self::check_tag_filter_with_normalization(filter, root_span))
        {
            return true;
        }

        if !conf
            .require_regex
            .iter()
            .all(|filter| Self::check_tag_filter_with_normalization(filter, root_span))
        {
            return true;
        }

        false
    }

    fn check_tag_filter_with_normalization<T: libdd_trace_utils::span::TraceData>(
        filter: &impl TagFilter,
        root_span: &libdd_trace_utils::span::v04::Span<T>,
    ) -> bool {
        let Some(value) = root_span.meta.get(filter.key()) else {
            return false;
        };
        let value = value.borrow();
        match filter.key() {
            "env" => {
                let normalized_value =
                    libdd_trace_normalization::normalize_utils::normalize_tag_cloned(value);
                filter.matches_tag_value(&normalized_value)
            }
            "http.status_code" => {
                if !libdd_trace_normalization::normalizer::is_valid_http_status_code(value) {
                    debug!(?value,"trace filter on http.status_code ignored because root span's `http.status_code` is invalid");
                    return false;
                }
                filter.matches_tag_value(value)
            }
            _ => filter.matches_tag_value(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_info::schema::FilterTagsConfig;
    use libdd_trace_utils::span::v04::SpanBytes;
    use std::collections::HashMap;

    // ---- helpers ----

    fn ftc(require: &[&str], reject: &[&str]) -> FilterTagsConfig {
        FilterTagsConfig {
            require: require.iter().map(|s| s.to_string()).collect(),
            reject: reject.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn no_tags() -> FilterTagsConfig {
        FilterTagsConfig::default()
    }

    fn span_with(resource: &'static str, meta: &[(&'static str, &'static str)]) -> SpanBytes {
        SpanBytes {
            service: "svc".into(),
            name: "op".into(),
            resource: resource.into(),
            span_id: 1,
            trace_id: 1,
            parent_id: 0,
            meta: meta
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect::<HashMap<_, _>>(),
            ..Default::default()
        }
    }

    fn one_trace(s: SpanBytes) -> Vec<Vec<SpanBytes>> {
        vec![vec![s]]
    }

    fn reject_str(tags: &[&str]) -> TraceFilterer {
        TraceFilterer::new(&ftc(&[], tags), &no_tags(), &[])
    }

    fn require_str(tags: &[&str]) -> TraceFilterer {
        TraceFilterer::new(&ftc(tags, &[]), &no_tags(), &[])
    }

    fn reject_regex(tags: &[&str]) -> TraceFilterer {
        TraceFilterer::new(&no_tags(), &ftc(&[], tags), &[])
    }

    fn require_regex(tags: &[&str]) -> TraceFilterer {
        TraceFilterer::new(&no_tags(), &ftc(tags, &[]), &[])
    }

    fn ignore_resources(patterns: &[&str]) -> TraceFilterer {
        let pats: Vec<String> = patterns.iter().map(|s| s.to_string()).collect();
        TraceFilterer::new(&no_tags(), &no_tags(), &pats)
    }

    // ---- reject (TagStringFilter) ----

    #[test]
    fn reject_string_exact_match_drops() {
        let mut traces = one_trace(span_with("r", &[("env", "prod")]));
        reject_str(&["env:prod"]).filter_traces(&mut traces);
        assert!(traces.is_empty());
    }

    #[test]
    fn reject_string_wrong_value_keeps() {
        let mut traces = one_trace(span_with("r", &[("env", "staging")]));
        reject_str(&["env:prod"]).filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }

    #[test]
    fn reject_string_missing_tag_keeps() {
        let mut traces = one_trace(span_with("r", &[]));
        reject_str(&["env:prod"]).filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }

    #[test]
    fn reject_string_key_only_matches_any_value() {
        // A key-only filter (no `:value` part) matches regardless of the tag's value.
        let mut traces = one_trace(span_with("r", &[("env", "anything")]));
        reject_str(&["env"]).filter_traces(&mut traces);
        assert!(traces.is_empty());
    }

    // ---- reject_regex (TagRegexFilter – literal key, regex value) ----

    #[test]
    fn reject_regex_value_match_drops() {
        let mut traces = one_trace(span_with("r", &[("env", "production")]));
        reject_regex(&["env:prod.*"]).filter_traces(&mut traces);
        assert!(traces.is_empty());
    }

    #[test]
    fn reject_regex_value_no_match_keeps() {
        let mut traces = one_trace(span_with("r", &[("env", "staging")]));
        reject_regex(&["env:prod.*"]).filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }

    // ---- reject_key_regex ----
    // Checks that it's not implemented

    #[test]
    fn reject_key_regex_key_and_value_match_drops() {
        let mut traces = one_trace(span_with("r", &[("error", "timeout")]));
        reject_regex(&["err.*:timeout"]).filter_traces(&mut traces);
        // Regex keys are not implemented so it doesn't match
        assert!(!traces.is_empty());
    }

    #[test]
    fn reject_key_regex_wrong_value_keeps() {
        let mut traces = one_trace(span_with("r", &[("error", "network")]));
        reject_regex(&["err.*:timeout"]).filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }

    #[test]
    fn reject_key_regex_missing_key_keeps() {
        let mut traces = one_trace(span_with("r", &[]));
        reject_regex(&["err.*:timeout"]).filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }

    // ---- require (TagStringFilter) ----

    #[test]
    fn require_string_present_and_matching_keeps() {
        let mut traces = one_trace(span_with("r", &[("env", "prod")]));
        require_str(&["env:prod"]).filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }

    #[test]
    fn require_string_missing_tag_drops() {
        let mut traces = one_trace(span_with("r", &[]));
        require_str(&["env:prod"]).filter_traces(&mut traces);
        assert!(traces.is_empty());
    }

    #[test]
    fn require_string_wrong_value_drops() {
        let mut traces = one_trace(span_with("r", &[("env", "staging")]));
        require_str(&["env:prod"]).filter_traces(&mut traces);
        assert!(traces.is_empty());
    }

    // ---- require_regex (TagRegexFilter – literal key, regex value) ----

    #[test]
    fn require_regex_value_match_keeps() {
        let mut traces = one_trace(span_with("r", &[("env", "production")]));
        require_regex(&["env:prod.*"]).filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }

    #[test]
    fn require_regex_missing_drops() {
        let mut traces = one_trace(span_with("r", &[]));
        require_regex(&["env:prod.*"]).filter_traces(&mut traces);
        assert!(traces.is_empty());
    }

    // ---- require_key_regex  ----
    // (Checks that it's not implemented)

    #[test]
    fn require_key_regex_key_exists_keeps() {
        let mut traces = one_trace(span_with("r", &[("error", "any")]));
        require_regex(&["err.*"]).filter_traces(&mut traces);
        // Regex keys are not implemented so it doesn't match
        assert!(traces.is_empty());
    }

    #[test]
    fn require_key_regex_missing_key_drops() {
        let mut traces = one_trace(span_with("r", &[]));
        require_regex(&["err.*"]).filter_traces(&mut traces);
        assert!(traces.is_empty());
    }

    // ---- ignore_resources ----

    #[test]
    fn ignore_resources_match_drops() {
        let mut traces = one_trace(span_with("GET /health", &[]));
        ignore_resources(&["GET /health"]).filter_traces(&mut traces);
        assert!(traces.is_empty());
    }

    #[test]
    fn ignore_resources_no_match_keeps() {
        let mut traces = one_trace(span_with("POST /data", &[]));
        ignore_resources(&["GET /health"]).filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }

    #[test]
    fn ignore_resources_empty_resource_falls_back_to_name() {
        // When resource is empty the span's name field is used for matching.
        // The helper sets name = "op", so ignore_resources("op") must drop it.
        let mut traces = one_trace(span_with("", &[]));
        ignore_resources(&["op"]).filter_traces(&mut traces);
        assert!(traces.is_empty());
    }

    // ---- env tag normalization ----

    #[test]
    fn env_normalization_reject_matches_after_lowercase() {
        // normalize_tag_cloned("PROD") == "prod"; the reject filter "env:prod" must fire.
        let mut traces = one_trace(span_with("r", &[("env", "PROD")]));
        reject_str(&["env:prod"]).filter_traces(&mut traces);
        assert!(
            traces.is_empty(),
            "env value should be normalized before matching"
        );
    }

    #[test]
    fn env_normalization_require_matches_normalized_value() {
        // normalize_tag_cloned("Prod Env") == "prod_env" (uppercase + space → underscore).
        let mut traces = one_trace(span_with("r", &[("env", "Prod Env")]));
        require_str(&["env:prod_env"]).filter_traces(&mut traces);
        assert_eq!(
            traces.len(),
            1,
            "normalized env should satisfy the require filter"
        );
    }

    // ---- http.status_code special handling ----

    #[test]
    fn http_status_code_invalid_value_skips_reject_filter() {
        // is_valid_http_status_code("abc") == false → check_tag_filter returns false
        // → reject never fires → trace kept even though the raw value equals the filter.
        let mut traces = one_trace(span_with("r", &[("http.status_code", "abc")]));
        reject_str(&["http.status_code:abc"]).filter_traces(&mut traces);
        assert_eq!(
            traces.len(),
            1,
            "invalid status code should not trigger the filter"
        );
    }

    #[test]
    fn http_status_code_valid_value_triggers_reject_filter() {
        let mut traces = one_trace(span_with("r", &[("http.status_code", "500")]));
        reject_str(&["http.status_code:500"]).filter_traces(&mut traces);
        assert!(traces.is_empty());
    }

    // ---- update_conf ----

    #[test]
    fn update_conf_takes_effect() {
        let f = TraceFilterer::new(&no_tags(), &no_tags(), &[]);

        // No filters: trace is kept.
        let mut traces = one_trace(span_with("r", &[("env", "prod")]));
        f.filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);

        // Swap in a reject filter: same trace is now dropped.
        f.update_conf(&ftc(&[], &["env:prod"]), &no_tags(), &[]);
        let mut traces = one_trace(span_with("r", &[("env", "prod")]));
        f.filter_traces(&mut traces);
        assert!(traces.is_empty());
    }

    // ---- edge / misc ----

    #[test]
    fn multiple_traces_partial_rejection() {
        let f = reject_str(&["env:prod"]);
        let mut traces = vec![
            vec![span_with("r", &[("env", "prod")])],    // dropped
            vec![span_with("r", &[("env", "staging")])], // kept
        ];
        f.filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }

    #[test]
    fn no_filters_keeps_all_traces() {
        let f = TraceFilterer::new(&no_tags(), &no_tags(), &[]);
        let mut traces = vec![
            vec![span_with("r1", &[])],
            vec![span_with("r2", &[("env", "prod")])],
        ];
        f.filter_traces(&mut traces);
        assert_eq!(traces.len(), 2);
    }

    #[test]
    fn invalid_regex_in_filter_is_skipped_gracefully() {
        // A bad regex pattern is silently discarded; no panic, trace is kept.
        let f = reject_regex(&["env:[invalid"]);
        let mut traces = one_trace(span_with("r", &[("env", "anything")]));
        f.filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }

    // ---- key/value trimming ----

    #[test]
    fn literal_reject_spaces_around_colon_drops() {
        // " env : prod " → key="env", value="prod"
        let mut traces = one_trace(span_with("r", &[("env", "prod")]));
        reject_str(&[" env : prod "]).filter_traces(&mut traces);
        assert!(traces.is_empty());
    }

    #[test]
    fn literal_require_spaces_around_colon_keeps() {
        let mut traces = one_trace(span_with("r", &[("env", "prod")]));
        require_str(&[" env : prod "]).filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }

    #[test]
    fn literal_reject_key_only_with_spaces_drops_any_value() {
        // " env " (no colon) → key="env", no value requirement
        let mut traces = one_trace(span_with("r", &[("env", "anything")]));
        reject_str(&[" env "]).filter_traces(&mut traces);
        assert!(traces.is_empty());
    }

    #[test]
    fn literal_reject_empty_key_is_skipped_keeps() {
        // ":prod" → key="" → filter skipped → trace kept
        let mut traces = one_trace(span_with("r", &[("env", "prod")]));
        reject_str(&[":prod"]).filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }

    #[test]
    fn literal_require_empty_key_is_skipped_keeps() {
        // ":prod" → filter skipped → require list empty → vacuous all() → trace kept
        let mut traces = one_trace(span_with("r", &[("env", "prod")]));
        require_str(&[":prod"]).filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }

    #[test]
    fn regex_reject_spaces_around_colon_drops() {
        // " env : prod.* " → key="env", regex="prod.*"
        let mut traces = one_trace(span_with("r", &[("env", "production")]));
        reject_regex(&[" env : prod.* "]).filter_traces(&mut traces);
        assert!(traces.is_empty());
    }

    #[test]
    fn regex_require_spaces_around_colon_keeps() {
        let mut traces = one_trace(span_with("r", &[("env", "production")]));
        require_regex(&[" env : prod.* "]).filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }

    #[test]
    fn regex_reject_empty_key_is_skipped_keeps() {
        // ":prod.*" → key="" → filter skipped → trace kept
        let mut traces = one_trace(span_with("r", &[("env", "prod")]));
        reject_regex(&[":prod.*"]).filter_traces(&mut traces);
        assert_eq!(traces.len(), 1);
    }
}
