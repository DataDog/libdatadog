// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! Trace-level filter logic for client-side stats (filter_tags, filter_tags_regex,
//! ignore_resources as published by the agent's /info endpoint).
use std::borrow::Borrow as _;

use libdd_common::regex_engine::Regex;
use libdd_trace_normalization::{normalize_utils, normalizer};
use tracing::{debug, error};

use crate::span::{self, trace_utils::get_root_span_index, TraceData};

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

/// Applies trace-level filters derived from the agent's `/info` endpoint configuration:
/// `filter_tags`, `filter_tags_regex`, and `ignore_resources`.
///
/// Filtering is evaluated on the root span of each trace.
#[derive(Debug, Default)]
pub struct TraceFilterer {
    reject: Vec<TagLiteralFilter>,
    reject_regex: Vec<TagRegexFilter>,

    require: Vec<TagLiteralFilter>,
    require_regex: Vec<TagRegexFilter>,

    ignore_resources: Vec<Regex>,
}

/// Minimal span interface required by [`TraceFilterer`].
pub trait Span<'a> {
    /// Returns the normalized resource value
    fn resource_normalized(&'a self) -> &'a str;
    /// Returns the value of the given meta tag, if present.
    fn get_meta(&'a self, key: &str) -> Option<&'a str>;
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

impl<'a, T: TraceData> Span<'a> for span::v04::Span<T> {
    fn resource_normalized(&'a self) -> &'a str {
        // Normalization
        let span_resource = self.resource.borrow();
        if span_resource.is_empty() {
            let span_name = self.name.borrow();
            debug!(
                ?span_name,
                "Trace filter: filtering on name because resource is empty"
            );
            span_name
        } else {
            span_resource
        }
    }

    fn get_meta(&'a self, key: &str) -> Option<&'a str> {
        self.meta.get(key).map(|v| v.borrow())
    }
}

impl TraceFilterer {
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

    /// Creates a new filterer from the agent's `/info` configuration fields.
    ///
    /// Invalid regex patterns are logged and skipped rather than causing an error.
    pub fn new(
        filter_tags_require: &[String],
        filter_tags_reject: &[String],
        filter_tags_regex_require: &[String],
        filter_tags_regex_reject: &[String],
        ignore_resources: &[String],
    ) -> Self {
        let require_regex = Self::compile_regex_filters(filter_tags_regex_require);
        let reject_regex = Self::compile_regex_filters(filter_tags_regex_reject);
        let require = Self::compile_literal_filters(filter_tags_require);
        let reject = Self::compile_literal_filters(filter_tags_reject);
        let ignore_resources = Self::compile_resource_filters(ignore_resources);

        Self {
            reject,
            require,
            reject_regex,
            require_regex,
            ignore_resources,
        }
    }
    /// Creates a no-op filterer that keeps all traces.
    pub fn with_empty_conf() -> Self {
        Self::default()
    }

    /// Removes traces that fail filter checks in-place. Returns the number of traces dropped.
    pub fn filter_traces(&self, traces: &mut Vec<Vec<span::v04::Span<impl TraceData>>>) -> usize {
        let traces_count_before = traces.len();
        traces.retain(|trace| {
            let Ok(root_span_index) = get_root_span_index(trace) else {
                return true;
            };
            let should_drop = self.should_drop(&trace[root_span_index]);
            if should_drop {
                debug!("Trace rejected as it fails to meet tag requirements. root: %v");
            }
            !should_drop
        });
        let traces_count_after = traces.len();

        traces_count_before - traces_count_after
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
    pub fn should_drop<'a>(&self, root_span: &'a impl Span<'a>) -> bool {
        if !self.ignore_resources.is_empty() {
            let span_resource = root_span.resource_normalized();

            if self
                .ignore_resources
                .iter()
                .any(|resource_pattern| resource_pattern.is_match(span_resource))
            {
                return true;
            }
        }

        if self
            .reject
            .iter()
            .any(|filter| Self::check_tag_filter_with_normalization(filter, root_span))
        {
            return true;
        }

        if self
            .reject_regex
            .iter()
            .any(|filter| Self::check_tag_filter_with_normalization(filter, root_span))
        {
            return true;
        }

        if !self
            .require
            .iter()
            .all(|filter| Self::check_tag_filter_with_normalization(filter, root_span))
        {
            return true;
        }

        if !self
            .require_regex
            .iter()
            .all(|filter| Self::check_tag_filter_with_normalization(filter, root_span))
        {
            return true;
        }

        false
    }

    fn check_tag_filter_with_normalization<'a>(
        filter: &impl TagFilter,
        root_span: &'a impl Span<'a>,
    ) -> bool {
        let Some(value) = root_span.get_meta(filter.key()) else {
            return false;
        };
        match filter.key() {
            "env" => {
                let normalized_value = normalize_utils::normalize_tag_cloned(value);
                filter.matches_tag_value(&normalized_value)
            }
            "http.status_code" => {
                if !normalizer::is_valid_http_status_code(value) {
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
    use super::TraceFilterer;
    use crate::span::v04::{SpanBytes, VecMap};
    // ---- helpers ----

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
                .collect::<VecMap<_, _>>(),
            ..Default::default()
        }
    }

    fn one_trace(s: SpanBytes) -> Vec<Vec<SpanBytes>> {
        vec![vec![s]]
    }

    fn map_to_owned(values: &[&str]) -> Vec<String> {
        values.iter().map(|&s| s.to_owned()).collect()
    }

    fn require_str(tags: &[&str]) -> TraceFilterer {
        TraceFilterer::new(&map_to_owned(tags), &[], &[], &[], &[])
    }

    fn reject_str(tags: &[&str]) -> TraceFilterer {
        TraceFilterer::new(&[], &map_to_owned(tags), &[], &[], &[])
    }

    fn require_regex(tags: &[&str]) -> TraceFilterer {
        TraceFilterer::new(&[], &[], &map_to_owned(tags), &[], &[])
    }

    fn reject_regex(tags: &[&str]) -> TraceFilterer {
        TraceFilterer::new(&[], &[], &[], &map_to_owned(tags), &[])
    }

    fn ignore_resources(patterns: &[&str]) -> TraceFilterer {
        TraceFilterer::new(&[], &[], &[], &[], &map_to_owned(patterns))
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

    #[cfg_attr(miri, ignore)] // regex compilation is prohibitively slow under Miri
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
        let f = TraceFilterer::new(&[], &[], &[], &[], &[]);
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

    #[cfg_attr(miri, ignore)] // regex compilation is prohibitively slow under Miri
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
