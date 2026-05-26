// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! Trace-level filter logic for client-side stats (filter_tags, filter_tags_regex,
//! ignore_resources as published by the agent's /info endpoint).
use std::{borrow::Borrow as _, collections::HashMap, sync::Arc};

use libdd_common::regex_engine;
use libdd_trace_stats::span_concentrator::StatSpan;
use libdd_trace_utils::span::trace_utils::get_root_span_index_v4;
use tracing::{debug, error};

trait TagFilter {
    /// Returns true if the given tag value matches the Filterer.
    fn matches_tag_value(&self, value: &str) -> bool;
    fn find_tag<'a, T: libdd_trace_utils::span::SpanText>(
        &'a self,
        meta: &'a HashMap<T, T>,
    ) -> Option<(&'a str, &'a T)>;
}

#[derive(Debug)]
struct TagStringFilter {
    key: String,
    value: Option<String>,
}

#[derive(Debug)]
struct TagRegexFilter {
    key: String,
    value: Option<regex_engine::Regex>,
}

#[derive(Debug)]
// Slowest kind of filter where the key field is also a regex
struct TagRegexKeyFilter {
    key: regex_engine::Regex,
    value: Option<regex_engine::Regex>,
}

/// Parsed config
#[derive(Debug)]
struct TraceFilteredConf {
    reject: Vec<TagStringFilter>,
    reject_regex: Vec<TagRegexFilter>,
    reject_key_regex: Vec<TagRegexKeyFilter>,

    require: Vec<TagStringFilter>,
    require_regex: Vec<TagRegexFilter>,
    require_key_regex: Vec<TagRegexKeyFilter>,

    ignore_resources: Vec<regex_engine::Regex>,
}

#[derive(Debug)]
pub struct TraceFilterer {
    conf: arc_swap::ArcSwap<TraceFilteredConf>,
}

impl TagStringFilter {
    fn from_str(tag: &str) -> Self {
        if let Some((key, value)) = tag.split_once(":") {
            TagStringFilter {
                key: key.to_owned(),
                value: Some(value.to_owned()),
            }
        } else {
            TagStringFilter {
                key: tag.to_owned(),
                value: None,
            }
        }
    }
}

impl TagFilter for TagStringFilter {
    fn matches_tag_value(&self, value: &str) -> bool {
        match &self.value {
            None => true, // No value requirement => Any value is a match
            Some(required_value) => value == required_value,
        }
    }

    fn find_tag<'a, T: libdd_trace_utils::span::SpanText>(
        &'a self,
        meta: &'a HashMap<T, T>,
    ) -> std::option::Option<(&'a str, &'a T)> {
        Some((self.key.as_ref(), meta.get(&self.key)?))
    }
}

impl TagFilter for TagRegexFilter {
    fn matches_tag_value(&self, value: &str) -> bool {
        match &self.value {
            None => true, // No value requirement => Any value is a match
            Some(pattern) => pattern.is_match(value),
        }
    }

    fn find_tag<'a, T: libdd_trace_utils::span::SpanText>(
        &'a self,
        meta: &'a HashMap<T, T>,
    ) -> std::option::Option<(&'a str, &'a T)> {
        Some((self.key.as_ref(), meta.get(&self.key)?))
    }
}

impl TagFilter for TagRegexKeyFilter {
    fn matches_tag_value(&self, value: &str) -> bool {
        match &self.value {
            None => true, // No value requirement => Any value is a match
            Some(pattern) => pattern.is_match(value),
        }
    }

    fn find_tag<'a, T: libdd_trace_utils::span::SpanText>(
        &self,
        meta: &'a HashMap<T, T>,
    ) -> std::option::Option<(&'a str, &'a T)> {
        meta.iter()
            .find(|&(key, _)| self.key.is_match(key.borrow()))
            .map(|(key, value)| (key.borrow(), value))
    }
}

/// Compile a regex anchored to the full string.
fn compile_anchored(pattern: &str) -> Result<regex_engine::Regex, regex_engine::Error> {
    regex_engine::Regex::new(&format!("^(?:{pattern})$"))
}

/// Returns `true` when `key` contains no regex metacharacters and can be used for a direct
/// O(1) lookup.  `.` is intentionally treated as a literal (not a wildcard) in key patterns.
fn is_literal_key(key: &str) -> bool {
    !key.contains([
        '*', '+', '?', '[', ']', '(', ')', '{', '}', '^', '$', ',', '\\',
    ])
}

impl TraceFilteredConf {
    /// Compile all `filter_tags_regex` entries, splitting into literal-key (fast) and
    /// regex-key (slow) lists based on whether the key portion contains metacharacters.
    fn compile_regex_filters(filters: &[String]) -> (Vec<TagRegexFilter>, Vec<TagRegexKeyFilter>) {
        let mut tag_regex_filters = Vec::new();
        let mut tag_regex_key_filters = Vec::new();
        for filter in filters {
            let (key, value) = match filter.split_once(":") {
                Some((key, value)) => (key, Some(value)),
                None => (filter.as_ref(), None),
            };

            let value = match value {
                Some(value) => match compile_anchored(value) {
                    Ok(regex) => Some(regex),
                    Err(err) => {
                        error!(
                            ?filter,
                            ?err,
                            "Invalid regex pattern in tag filter's value, skipping it"
                        );
                        // FIXME: dd-trace-php considers that if the value pattern is bad, we still
                        // keep the filter by only matching on the key. I find it more intuitive to
                        // drop the filter altogether
                        continue;
                    }
                },
                None => None,
            };

            if is_literal_key(key) {
                tag_regex_filters.push(TagRegexFilter {
                    key: key.to_owned(),
                    value,
                });
            } else {
                match compile_anchored(key) {
                    Ok(key) => tag_regex_key_filters.push(TagRegexKeyFilter { key, value }),
                    Err(err) => {
                        error!(
                            ?filter,
                            ?err,
                            "Invalid regex pattern in tag filter's key, skipping it"
                        );
                        continue;
                    }
                }
            }
        }

        (tag_regex_filters, tag_regex_key_filters)
    }

    fn parse(
        filter_tags: &crate::agent_info::schema::FilterTagsConfig,
        filter_tags_regex: &crate::agent_info::schema::FilterTagsConfig,
        ignore_resources: &[String],
    ) -> Self {
        let (require_regex, require_key_regex) =
            Self::compile_regex_filters(&filter_tags_regex.require);
        let (reject_regex, reject_key_regex) =
            Self::compile_regex_filters(&filter_tags_regex.reject);

        let reject = filter_tags
            .reject
            .iter()
            .map(|tag| TagStringFilter::from_str(tag))
            .collect();
        let require = filter_tags
            .require
            .iter()
            .map(|tag| TagStringFilter::from_str(tag))
            .collect();
        let ignore_resources = ignore_resources
            .iter()
            .filter_map(|regex| {
                compile_anchored(regex)
                    .inspect_err(|err| {
                        error!(
                            ?regex,
                            ?err,
                            "Invalid regex pattern in ignore resources filter, skipping it"
                        )
                    })
                    .ok()
            })
            .collect();
        TraceFilteredConf {
            reject,
            require,
            reject_regex,
            require_regex,
            reject_key_regex,
            require_key_regex,
            ignore_resources,
        }
    }
}

impl TraceFilterer {
    pub fn new(
        filter_tags: &crate::agent_info::schema::FilterTagsConfig,
        filter_tags_regex: &crate::agent_info::schema::FilterTagsConfig,
        ignore_resources: &[String],
    ) -> Self {
        let conf = TraceFilteredConf::parse(filter_tags, filter_tags_regex, ignore_resources);
        Self {
            conf: arc_swap::ArcSwap::from_pointee(conf),
        }
    }

    pub fn update_conf(
        &self,
        filter_tags: &crate::agent_info::schema::FilterTagsConfig,
        filter_tags_regex: &crate::agent_info::schema::FilterTagsConfig,
        ignore_resources: &[String],
    ) {
        let new_conf = TraceFilteredConf::parse(filter_tags, filter_tags_regex, ignore_resources);
        self.conf.swap(Arc::new(new_conf));
    }

    pub fn filter_traces<T: libdd_trace_utils::span::TraceData>(
        &self,
        traces: &mut Vec<Vec<libdd_trace_utils::span::v04::Span<T>>>,
    ) {
        let conf = self.conf.load();
        traces.retain(|trace| {
            let Ok(root_span_index) = get_root_span_index_v4(trace) else {
                // FIXME: in this case it's a distributed trace ? Maybe we should remove the debug
                // log in get_root_span_index_v4 then
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
    fn should_drop<T: libdd_trace_utils::span::TraceData>(
        conf: &TraceFilteredConf,
        root_span: &libdd_trace_utils::span::v04::Span<T>,
    ) -> bool {
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

        if conf
            .reject_key_regex
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

        if !conf
            .require_key_regex
            .iter()
            .all(|filter| Self::check_tag_filter_with_normalization(filter, root_span))
        {
            return true;
        }

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

        false
    }

    fn check_tag_filter_with_normalization<T: libdd_trace_utils::span::TraceData>(
        filter: &impl TagFilter,
        root_span: &libdd_trace_utils::span::v04::Span<T>,
    ) -> bool {
        let Some((key, value)) = filter.find_tag(&root_span.meta) else {
            return false;
        };
        let value = value.borrow();
        match key {
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
