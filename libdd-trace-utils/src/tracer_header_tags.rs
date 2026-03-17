// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use http::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};

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

#[derive(Default, Debug, Serialize, Deserialize, Clone)]
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
    // number of trace chunks dropped in the tracer
    pub dropped_p0_traces: usize,
    // number of spans dropped in the tracer
    pub dropped_p0_spans: usize,
}

impl<'a> From<TracerHeaderTags<'a>> for HeaderMap {
    fn from(tags: TracerHeaderTags<'a>) -> HeaderMap {
        let mut headers = HeaderMap::with_capacity(10);
        fn try_insert(
            h: &mut HeaderMap,
            key: HeaderName,
            v: impl TryInto<HeaderValue> + AsRef<[u8]>,
        ) {
            if v.as_ref().is_empty() {
                return;
            }
            if let Ok(v) = v.try_into() {
                h.insert(key, v);
            }
        }
        try_insert(
            &mut headers,
            HeaderName::from_static("datadog-meta-lang"),
            tags.lang,
        );
        try_insert(
            &mut headers,
            HeaderName::from_static("datadog-meta-lang-version"),
            tags.lang_version,
        );
        try_insert(
            &mut headers,
            HeaderName::from_static("datadog-meta-lang-interpreter"),
            tags.lang_interpreter,
        );
        try_insert(
            &mut headers,
            HeaderName::from_static("datadog-meta-lang-interpreter-vendor"),
            tags.lang_vendor,
        );
        try_insert(
            &mut headers,
            HeaderName::from_static("datadog-meta-tracer-version"),
            tags.tracer_version,
        );
        try_insert(
            &mut headers,
            HeaderName::from_static("datadog-container-id"),
            tags.container_id,
        );
        if tags.client_computed_stats {
            try_insert(
                &mut headers,
                HeaderName::from_static("datadog-client-computed-stats"),
                HeaderValue::from_static("true"),
            );
        }
        if tags.client_computed_top_level {
            try_insert(
                &mut headers,
                HeaderName::from_static("datadog-client-computed-top-level"),
                HeaderValue::from_static("true"),
            );
        }
        if tags.dropped_p0_traces > 0 {
            try_insert(
                &mut headers,
                HeaderName::from_static("datadog-client-dropped-p0-traces"),
                tags.dropped_p0_traces.to_string(),
            );
        }
        if tags.dropped_p0_spans > 0 {
            try_insert(
                &mut headers,
                HeaderName::from_static("datadog-client-dropped-p0-spans"),
                tags.dropped_p0_spans.to_string(),
            );
        }
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
                "datadog-meta-lang-interpreter-vendor" => tags.lang_vendor,
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
        if let Some(count) = headers.get("datadog-client-dropped-p0-traces") {
            tags.dropped_p0_traces = count
                .to_str()
                .unwrap_or_default()
                .parse()
                .unwrap_or_default();
        }
        if let Some(count) = headers.get("datadog-client-dropped-p0-spans") {
            tags.dropped_p0_spans = count.to_str().map_or(0, |c| c.parse().unwrap_or(0));
        }
        tags
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get<'a>(m: &'a HeaderMap, key: &str) -> Option<&'a str> {
        m.get(key).and_then(|v| v.to_str().ok())
    }

    #[test]
    fn tags_to_hashmap() {
        let header_tags = TracerHeaderTags {
            lang: "test-lang",
            lang_version: "2.0",
            lang_interpreter: "interpreter",
            lang_vendor: "vendor",
            tracer_version: "1.0",
            container_id: "id",
            client_computed_top_level: true,
            client_computed_stats: true,
            dropped_p0_traces: 12,
            dropped_p0_spans: 120,
        };

        let map: HeaderMap = header_tags.into();

        assert_eq!(map.len(), 10);
        assert_eq!(get(&map, "datadog-meta-lang"), Some("test-lang"));
        assert_eq!(get(&map, "datadog-meta-lang-version"), Some("2.0"));
        assert_eq!(
            get(&map, "datadog-meta-lang-interpreter"),
            Some("interpreter")
        );
        assert_eq!(
            get(&map, "datadog-meta-lang-interpreter-vendor"),
            Some("vendor")
        );
        assert_eq!(get(&map, "datadog-meta-tracer-version"), Some("1.0"));
        assert_eq!(get(&map, "datadog-container-id"), Some("id"));
        assert_eq!(get(&map, "datadog-client-computed-top-level"), Some("true"));
        assert_eq!(get(&map, "datadog-client-computed-stats"), Some("true"));
        assert_eq!(get(&map, "datadog-client-dropped-p0-traces"), Some("12"));
        assert_eq!(get(&map, "datadog-client-dropped-p0-spans"), Some("120"));
    }

    #[test]
    fn tags_to_hashmap_empty_value() {
        let header_tags = TracerHeaderTags {
            lang: "test-lang",
            lang_version: "2.0",
            lang_interpreter: "interpreter",
            lang_vendor: "vendor",
            tracer_version: "1.0",
            container_id: "",
            client_computed_top_level: false,
            client_computed_stats: false,
            dropped_p0_spans: 0,
            dropped_p0_traces: 0,
        };

        let map: HeaderMap = header_tags.into();

        assert_eq!(map.len(), 5);
        assert_eq!(get(&map, "datadog-meta-lang"), Some("test-lang"));
        assert_eq!(get(&map, "datadog-meta-lang-version"), Some("2.0"));
        assert_eq!(
            get(&map, "datadog-meta-lang-interpreter"),
            Some("interpreter")
        );
        assert_eq!(
            get(&map, "datadog-meta-lang-interpreter-vendor"),
            Some("vendor")
        );
        assert_eq!(get(&map, "datadog-meta-tracer-version"), Some("1.0"));
        assert_eq!(get(&map, "datadog-container-id"), None);
        assert_eq!(get(&map, "datadog-client-computed-top-level"), None);
        assert_eq!(get(&map, "datadog-client-computed-stats"), None);
        assert_eq!(get(&map, "datadog-client-dropped-p0-traces"), None);
        assert_eq!(get(&map, "datadog-client-dropped-p0-spans"), None);
    }

    #[test]
    fn header_map_to_tags() {
        let mut header_map = HeaderMap::new();

        header_map.insert("datadog-meta-lang", "test-lang".parse().unwrap());
        header_map.insert("datadog-meta-lang-version", "2.0".parse().unwrap());
        header_map.insert(
            "datadog-meta-lang-interpreter",
            "interpreter".parse().unwrap(),
        );
        header_map.insert(
            "datadog-meta-lang-interpreter-vendor",
            "vendor".parse().unwrap(),
        );
        header_map.insert("datadog-meta-tracer-version", "1.0".parse().unwrap());
        header_map.insert("datadog-container-id", "id".parse().unwrap());
        header_map.insert("datadog-client-computed-stats", "true".parse().unwrap());
        header_map.insert("datadog-client-dropped-p0-traces", "12".parse().unwrap());

        let tags: TracerHeaderTags = (&header_map).into();

        assert_eq!(tags.lang, "test-lang");
        assert_eq!(tags.lang_vendor, "vendor");
        assert_eq!(tags.lang_version, "2.0");
        assert_eq!(tags.tracer_version, "1.0");
        assert_eq!(tags.lang_interpreter, "interpreter");
        assert_eq!(tags.container_id, "id");
        assert!(tags.client_computed_stats);
        assert!(!tags.client_computed_top_level);
        assert_eq!(tags.dropped_p0_traces, 12);
        assert_eq!(tags.dropped_p0_spans, 0);
    }
}
