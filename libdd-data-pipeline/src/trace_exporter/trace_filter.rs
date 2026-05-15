// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
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
struct RegexTagFilter {
    key: String,
    value: Option<regex_engine::Regex>,
}

/// Parsed config
#[derive(Debug)]
struct TraceFilteredConf {
    reject: Vec<TagFilter>,
    reject_regex: Vec<RegexTagFilter>,
    require: Vec<TagFilter>,
    require_regex: Vec<RegexTagFilter>,
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

impl FromStr for RegexTagFilter {
    type Err = regex_engine::Error;

    fn from_str(tag: &str) -> Result<Self, Self::Err> {
        if let Some((key, value)) = tag.split_once(":") {
            let regex = match regex_engine::Regex::new(value) {
                Ok(regex) => regex,
                Err(err) => {
                    error!(
                        "Invalid regex pattern in tag filter, skipping it: tag=`{tag}` err={err}"
                    );
                    return Err(err);
                }
            };
            Ok(RegexTagFilter {
                key: key.to_owned(),
                value: Some(regex),
            })
        } else {
            Ok(RegexTagFilter {
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
                .filter_map(|regex_tag| RegexTagFilter::from_str(regex_tag).ok())
                .collect(),
            require: filter_tags
                .require
                .iter()
                .map(|tag| TagFilter::from_str(tag))
                .collect(),
            require_regex: filter_tags_regex
                .require
                .iter()
                .filter_map(|regex_tag| RegexTagFilter::from_str(regex_tag).ok())
                .collect(),
            ignore_resources: ignore_resources
                .iter()
                .filter_map(|regex| {
                    regex_engine::Regex::new(regex).inspect_err(|err| {
                    error!("Invalid regex pattern in ignore resources filter, skipping it: regex=`{regex}` err={err}")
                }).ok()
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
        traces.retain(|trace| {
            let Ok(root_span_index) = get_root_span_index_v4(trace) else {
                // FIXME: in this case it's a distributed trace ? Maybe we should remove the debug
                // log in get_root_span_index_v4 then
                return true;
            };
            let root_span = &trace[root_span_index];
            let should_drop = self.should_drop(root_span);
            if should_drop {
                debug!("Trace rejected as it fails to meet tag requirements. root: %v");
            }
            !should_drop
        });
    }

    fn should_drop<T: libdd_trace_utils::span::TraceData>(
        &self,
        root_span: &libdd_trace_utils::span::v04::Span<T>,
    ) -> bool {
        let conf = self.conf.load();
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

        if conf
            .ignore_resources
            .iter()
            .any(|resource_pattern| resource_pattern.is_match(root_span.resource()))
        {
            return true;
        }

        false
    }
}
