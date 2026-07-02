// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::tracer_header_tags::TracerHeaderTags;
use http::HeaderMap;

#[derive(Clone, Default, Debug)]
pub struct TracerMetadata {
    pub hostname: String,
    pub env: String,
    pub app_version: String,
    pub runtime_id: String,
    pub service: String,
    pub tracer_version: String,
    pub language: String,
    pub language_version: String,
    pub language_interpreter: String,
    pub language_interpreter_vendor: String,
    pub container_id: String,
    pub git_commit_sha: String,
    pub process_tags: String,
    pub client_computed_stats: bool,
    pub client_computed_top_level: bool,
}

impl<'a> From<&'a TracerMetadata> for TracerHeaderTags<'a> {
    fn from(tags: &'a TracerMetadata) -> TracerHeaderTags<'a> {
        TracerHeaderTags::<'_> {
            lang: &tags.language,
            lang_version: &tags.language_version,
            tracer_version: &tags.tracer_version,
            lang_interpreter: &tags.language_interpreter,
            lang_vendor: &tags.language_interpreter_vendor,
            container_id: &tags.container_id,
            client_computed_stats: tags.client_computed_stats,
            client_computed_top_level: tags.client_computed_top_level,
            ..Default::default()
        }
    }
}

impl<'a> From<&'a TracerMetadata> for HeaderMap {
    fn from(tags: &'a TracerMetadata) -> HeaderMap {
        TracerHeaderTags::from(tags).into()
    }
}
