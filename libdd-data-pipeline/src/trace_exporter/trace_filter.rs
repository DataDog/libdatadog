// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! Trace-level filter logic for client-side stats (filter_tags, filter_tags_regex,
//! ignore_resources as published by the agent's /info endpoint).
use std::{str::FromStr, sync::Arc};

use libdd_common::regex_engine;
use libdd_trace_stats::span_concentrator::StatSpan;
use libdd_trace_utils::span::trace_utils::get_root_span_index_v4;
use tracing::{debug, error};

#[derive(Debug)]
struct TagFilter {
    key: String,
    value: Option<String>,
}

#[derive(Debug)]
struct TagRegexFilter {
    key: String,
    value: Option<regex_engine::Regex>,
}

// #[derive(Debug)]
// // Slowest kind of filter where the key field is also a regex
// struct TagRegexKeyFilter {
//     key: regex_engine::Regex,
//     value: Option<regex_engine::Regex>,
// }

/// Parsed config
#[derive(Debug)]
struct TraceFilteredConf {
    reject: Vec<TagFilter>,
    reject_regex: Vec<TagRegexFilter>,
    require: Vec<TagFilter>,
    require_regex: Vec<TagRegexFilter>,
    ignore_resources: Vec<regex_engine::Regex>,
}

#[derive(Debug)]
pub struct TraceFilterer {
    conf: arc_swap::ArcSwap<TraceFilteredConf>,
}

impl TagFilter {
    fn from_str(tag: &str) -> Self {
        if let Some((key, value)) = tag.split_once(":") {
            TagFilter {
                key: key.to_owned(),
                value: Some(value.to_owned()),
            }
        } else {
            TagFilter {
                key: tag.to_owned(),
                value: None,
            }
        }
    }
}

impl FromStr for TagRegexFilter {
    type Err = regex_engine::Error;

    fn from_str(tag: &str) -> Result<Self, Self::Err> {
        if let Some((key, value)) = tag.split_once(":") {
            let regex = match regex_engine::Regex::new(value) {
                Ok(regex) => regex,
                Err(err) => {
                    error!(
                        ?tag,
                        ?err,
                        "Invalid regex pattern in tag filter, skipping it"
                    );
                    return Err(err);
                }
            };
            Ok(TagRegexFilter {
                key: key.to_owned(),
                value: Some(regex),
            })
        } else {
            Ok(TagRegexFilter {
                key: tag.to_owned(),
                value: None,
            })
        }
    }
}

impl TraceFilteredConf {
    fn parse(
        filter_tags: &crate::agent_info::schema::FilterTagsConfig,
        filter_tags_regex: &crate::agent_info::schema::FilterTagsConfig,
        ignore_resources: &[String],
    ) -> Self {
        TraceFilteredConf {
            reject: filter_tags
                .reject
                .iter()
                .map(|tag| TagFilter::from_str(tag))
                .collect(),
            reject_regex: filter_tags_regex
                .reject
                .iter()
                .filter_map(|regex_tag| TagRegexFilter::from_str(regex_tag).ok())
                .collect(),
            require: filter_tags
                .require
                .iter()
                .map(|tag| TagFilter::from_str(tag))
                .collect(),
            require_regex: filter_tags_regex
                .require
                .iter()
                .filter_map(|regex_tag| TagRegexFilter::from_str(regex_tag).ok())
                .collect(),
            ignore_resources: ignore_resources
                .iter()
                .filter_map(|regex| {
                    regex_engine::Regex::new(regex)
                        .inspect_err(|err| {
                            error!(
                                ?regex,
                                ?err,
                                "Invalid regex pattern in ignore resources filter, skipping it"
                            )
                        })
                        .ok()
                })
                .collect(),
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

    fn should_drop<T: libdd_trace_utils::span::TraceData>(
        conf: &TraceFilteredConf,
        root_span: &libdd_trace_utils::span::v04::Span<T>,
    ) -> bool {
        let has_tag_filters = !conf.reject.is_empty()
            || !conf.reject_regex.is_empty()
            || !conf.require.is_empty()
            || !conf.require_regex.is_empty();
        if has_tag_filters {
            // let env_tag = root_span
            //     .meta
            //     .get("env")
            //     .map(|v| libdd_trace_normalization::normalize_utils::normalize_tag(v.borrow()));

            // if let Some(code) = s.meta.get("http.status_code") {
            //     if !is_valid_status_code(code) {
            //         s.meta.remove("http.status_code");
            //     }
            // };

            if conf.reject.iter().any(|tag| {
                root_span
                    .get_meta(&tag.key)
                    .is_some_and(|value| tag.value.as_ref().is_none_or(|v| v == value))
            }) {
                return true;
            }

            if conf.reject_regex.iter().any(|tag| {
                root_span
                    .get_meta(&tag.key)
                    .is_some_and(|value| tag.value.as_ref().is_none_or(|pat| pat.is_match(value)))
            }) {
                return true;
            }

            if !conf.require.iter().all(|tag| {
                root_span
                    .get_meta(&tag.key)
                    .is_some_and(|value| tag.value.as_ref().is_none_or(|v| v == value))
            }) {
                return true;
            }

            if !conf.require_regex.iter().all(|tag| {
                root_span
                    .get_meta(&tag.key)
                    .is_some_and(|value| tag.value.as_ref().is_none_or(|pat| pat.is_match(value)))
            }) {
                return true;
            }
        }

        if !conf.ignore_resources.is_empty() {
            let span_resource = root_span.resource();
            // Normalization
            let span_resource = if span_resource.is_empty() {
                let span_name = root_span.name();
                debug!(
                    ?span_name,
                    "Fixing malformed trace. Resource is empty setting span.resource=span.name"
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
}
