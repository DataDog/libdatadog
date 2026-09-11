// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::normalize_utils;
use libdd_trace_protobuf::pb;
use libdd_trace_types::span::SpanText;
use libdd_trace_types::span::TraceData;
use std::borrow::Borrow;

const TAG_SAMPLING_PRIORITY: &str = "_sampling_priority_v1";
const TAG_ORIGIN: &str = "_dd.origin";

#[allow(dead_code)]
#[derive(Debug, Eq, PartialEq)]
pub enum SamplerPriority {
    AutoDrop = 0,
    AutoKeep = 1,
    UserKeep = 2,
    None = i8::MIN as isize,
}

pub(crate) fn normalize_span(s: &mut pb::Span) -> anyhow::Result<()> {
    anyhow::ensure!(s.trace_id != 0, "TraceID is zero (reason:trace_id_zero)");
    anyhow::ensure!(s.span_id != 0, "SpanID is zero (reason:span_id_zero)");

    // TODO: component2name: check for a feature flag to determine the component tag to become the
    // span name https://github.com/DataDog/datadog-agent/blob/dc88d14851354cada1d15265220a39dce8840dcc/pkg/trace/agent/normalizer.go#L64

    normalize_utils::normalize_service(&mut s.service);
    normalize_utils::normalize_name(&mut s.name);
    normalize_utils::normalize_resource(&mut s.resource, &s.name);
    normalize_utils::normalize_parent_id(&mut s.parent_id, s.trace_id, s.span_id);
    normalize_utils::normalize_span_start_duration(&mut s.start, &mut s.duration);
    normalize_utils::normalize_span_type(&mut s.r#type);

    if let Some(env_tag) = s.meta.get_mut("env") {
        normalize_utils::normalize_tag(env_tag);
    }

    if let Some(code) = s.meta.get("http.status_code") {
        if !is_valid_http_status_code(code) {
            s.meta.remove("http.status_code");
        }
    };

    Ok(())
}

pub fn is_valid_http_status_code(sc: &str) -> bool {
    if let Ok(code) = sc.parse::<i64>() {
        return (100..600).contains(&code);
    }
    false
}

/// normalize_trace takes a trace and
/// * returns an error if there is a trace ID discrepancy between 2 spans
/// * returns an error if at least one span cannot be normalized
pub fn normalize_trace(trace: &mut [pb::Span]) -> anyhow::Result<()> {
    let first_trace_id = match trace.first() {
        Some(first_span) => first_span.trace_id,
        None => anyhow::bail!("Normalize Trace Error: Trace is empty"),
    };

    for span in trace {
        if span.trace_id != first_trace_id {
            anyhow::bail!(format!(
                "Normalize Trace Error: Trace has foreign span: {:?}",
                span
            ));
        }
        normalize_span(span)?;
    }
    Ok(())
}

/// Private generic helper: allocates one String, normalizes it, compares directly, replaces if
/// changed.
fn normalize_span_text_field<S: SpanText, F>(field: &mut S, normalizer: F)
where
    F: FnOnce(&mut String),
{
    let mut normalized = <S as Borrow<str>>::borrow(field).to_owned();
    normalizer(&mut normalized);
    if normalized != *<S as Borrow<str>>::borrow(field) {
        *field = S::from_owned(normalized);
    }
}

/// normalize_trace_v04 takes a v0.4 trace and:
/// * validates the trace is nonempty
/// * validates all spans share one consistent u128 trace ID
/// * validates all spans have nonzero span IDs
/// * normalizes service, name, resource, parent ID, start/duration, type, env, and invalid
///   http.status_code
/// * replaces text only when normalization changes it
pub fn normalize_trace_v04<T: TraceData>(
    trace: &mut [libdd_trace_types::span::v04::Span<T>],
) -> anyhow::Result<()> {
    // Validate trace is not empty
    let first_span = match trace.first() {
        Some(span) => span,
        None => anyhow::bail!("Normalize Trace Error: Trace is empty"),
    };

    let first_trace_id = first_span.trace_id;
    anyhow::ensure!(
        first_trace_id != 0,
        "TraceID is zero (reason:trace_id_zero)"
    );

    // Validate consistency: all spans have same trace ID and nonzero span ID
    for span in trace.iter() {
        anyhow::ensure!(
            span.trace_id == first_trace_id,
            "Normalize Trace Error: Trace has foreign span with trace_id {}",
            span.trace_id
        );
        anyhow::ensure!(span.span_id != 0, "SpanID is zero (reason:span_id_zero)");
    }

    // Normalize each span
    for span in trace.iter_mut() {
        normalize_v04_span(span, first_trace_id);
    }

    Ok(())
}

fn normalize_v04_span<T: TraceData>(
    span: &mut libdd_trace_types::span::v04::Span<T>,
    trace_id: u128,
) {
    // Normalize service using helper
    normalize_span_text_field(&mut span.service, normalize_utils::normalize_service);

    // Normalize name using helper
    normalize_span_text_field(&mut span.name, normalize_utils::normalize_name);

    // Normalize resource using helper: borrow span.name, allocate one String for resource, compare,
    // replace if different
    let name_ref = <T::Text as Borrow<str>>::borrow(&span.name);
    let mut resource_str = <T::Text as Borrow<str>>::borrow(&span.resource).to_owned();
    normalize_utils::normalize_resource(&mut resource_str, name_ref);
    if resource_str != *<T::Text as Borrow<str>>::borrow(&span.resource) {
        span.resource = T::Text::from_owned(resource_str);
    }

    // Normalize parent_id using u128 trace_id comparison for Zipkin root span detection
    let parent_id_as_u128 = u128::from(span.parent_id);
    if parent_id_as_u128 == trace_id && trace_id == u128::from(span.span_id) {
        span.parent_id = 0;
    }

    // Normalize start and duration
    normalize_utils::normalize_span_start_duration(&mut span.start, &mut span.duration);

    // Normalize type using helper
    normalize_span_text_field(&mut span.r#type, normalize_utils::normalize_span_type);

    // Normalize env meta tag if present using helper
    if let Some(env_val) = span.meta.get_mut("env") {
        normalize_span_text_field(env_val, normalize_utils::normalize_tag);
    }

    // Remove invalid http.status_code
    if let Some(code_val) = span.meta.get("http.status_code") {
        if !is_valid_http_status_code(code_val.borrow()) {
            span.meta.remove_slow("http.status_code");
        }
    }
}

/// normalize_chunk takes a trace chunk and
/// * populates origin field if it wasn't populated
/// * populates priority field if it wasn't populated the root span is used to populate these
///   fields, and it's index in TraceChunk spans vec must be passed.
pub fn normalize_chunk(chunk: &mut pb::TraceChunk, root_span_index: usize) -> anyhow::Result<()> {
    // check if priority is not populated
    let root_span = match chunk.spans.get(root_span_index) {
        Some(span) => span,
        None => {
            anyhow::bail!("Normalize Chunk Error: root_span_index > length of trace chunk spans")
        }
    };

    if chunk.priority == SamplerPriority::None as i32 {
        // Older tracers set sampling priority in the root span.
        if let Some(root_span_priority) = root_span.metrics.get(TAG_SAMPLING_PRIORITY) {
            chunk.priority = *root_span_priority as i32;
        } else {
            for span in &chunk.spans {
                if let Some(priority) = span.metrics.get(TAG_SAMPLING_PRIORITY) {
                    chunk.priority = *priority as i32;
                    break;
                }
            }
        }
    }
    // check if origin is not populated
    if chunk.origin.is_empty() {
        if let Some(origin) = root_span.meta.get(TAG_ORIGIN) {
            // Older tracers set origin in the root span.
            chunk.origin = origin.to_string();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::normalize_utils;
    use crate::normalize_utils::{DEFAULT_SPAN_NAME, MAX_TYPE_LEN};
    use crate::normalizer;
    use libdd_trace_protobuf::pb;
    use rand::Rng;
    use std::collections::HashMap;
    use std::time::SystemTime;

    fn new_test_span() -> pb::Span {
        let mut rng = rand::thread_rng();

        pb::Span {
            duration: 10000000,
            error: 0,
            resource: "GET /some/raclette".to_string(),
            service: "django".to_string(),
            name: "django.controller".to_string(),
            span_id: rng.gen(),
            start: 1448466874000000000,
            trace_id: 424242,
            meta: HashMap::from([
                ("user".to_string(), "leo".to_string()),
                ("pool".to_string(), "fondue".to_string()),
            ]),
            metrics: HashMap::from([("cheese_weight".to_string(), 100000.0)]),
            parent_id: 1111,
            r#type: "http".to_string(),
            meta_struct: HashMap::new(),
            span_links: vec![],
            span_events: vec![],
        }
    }

    fn new_test_chunk_with_span(span: pb::Span) -> pb::TraceChunk {
        pb::TraceChunk {
            priority: 1,
            origin: "".to_string(),
            spans: vec![span],
            tags: HashMap::new(),
            dropped_trace: false,
        }
    }

    #[test]
    fn test_normalize_name_passes() {
        let mut test_span = new_test_span();
        let before_name = test_span.name.clone();
        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(before_name, test_span.name);
    }

    #[test]
    fn test_normalize_empty_name() {
        let mut test_span = new_test_span();
        test_span.name = "".to_string();
        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(test_span.name, DEFAULT_SPAN_NAME);
    }

    #[test]
    fn test_normalize_long_name() {
        let mut test_span = new_test_span();
        test_span.name = "CAMEMBERT".repeat(100);
        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert!(test_span.name.len() == normalize_utils::MAX_NAME_LEN);
    }

    #[test]
    fn test_normalize_name_no_alphanumeric() {
        let mut test_span = new_test_span();
        test_span.name = "/".to_string();
        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(test_span.name, DEFAULT_SPAN_NAME);
    }

    #[test]
    fn test_normalize_name_for_metrics() {
        let expected_names = HashMap::from([
            (
                "pylons.controller".to_string(),
                "pylons.controller".to_string(),
            ),
            (
                "trace-api.request".to_string(),
                "trace_api.request".to_string(),
            ),
        ]);

        let mut test_span = new_test_span();
        for (name, expected_name) in expected_names {
            test_span.name = name;
            assert!(normalizer::normalize_span(&mut test_span).is_ok());
            assert_eq!(test_span.name, expected_name);
        }
    }

    #[test]
    fn test_normalize_resource_passes() {
        let mut test_span = new_test_span();
        let before_resource = test_span.resource.clone();
        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(before_resource, test_span.resource);
    }

    #[test]
    fn test_normalize_empty_resource() {
        let mut test_span = new_test_span();
        test_span.resource = "".to_string();
        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(test_span.resource, test_span.name);
    }

    #[test]
    fn test_normalize_trace_id_passes() {
        let mut test_span = new_test_span();
        let before_trace_id = test_span.trace_id;
        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(before_trace_id, test_span.trace_id);
    }

    #[test]
    fn test_normalize_no_trace_id() {
        let mut test_span = new_test_span();
        test_span.trace_id = 0;
        assert!(normalizer::normalize_span(&mut test_span).is_err());
    }

    #[test]
    fn test_normalize_component_to_name() {
        let mut test_span = new_test_span();
        let before_trace_id = test_span.trace_id;
        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(before_trace_id, test_span.trace_id);
    }

    // TODO: Add a unit test for testing Component2Name, one that is
    //       implemented within the normalize function.

    #[test]
    fn test_normalize_span_id_passes() {
        let mut test_span = new_test_span();
        let before_span_id = test_span.span_id;
        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(before_span_id, test_span.span_id);
    }

    #[test]
    fn test_normalize_no_span_id() {
        let mut test_span = new_test_span();
        test_span.span_id = 0;
        assert!(normalizer::normalize_span(&mut test_span).is_err());
    }

    #[test]
    fn test_normalize_start_passes() {
        let mut test_span = new_test_span();
        let before_start = test_span.start;
        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(before_start, test_span.start);
    }

    fn get_current_time() -> i64 {
        SystemTime::UNIX_EPOCH.elapsed().unwrap().as_nanos() as i64
    }

    #[test]
    fn test_normalize_start_too_small() {
        let mut test_span = new_test_span();

        test_span.start = 42;
        let min_start = get_current_time() - test_span.duration;

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert!(test_span.start >= min_start);
        assert!(test_span.start <= get_current_time());
    }

    #[test]
    fn test_normalize_start_too_small_with_large_duration() {
        let mut test_span = new_test_span();

        test_span.start = 42;
        test_span.duration = get_current_time() * 2;
        let min_start = get_current_time();

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert!(test_span.start >= min_start); // start should have been reset to current time
        assert!(test_span.start <= get_current_time()); //start should have been reset to current
                                                        // time
    }

    #[test]
    fn test_normalize_duration_passes() {
        let mut test_span = new_test_span();
        let before_duration = test_span.duration;

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(before_duration, test_span.duration);
    }

    #[test]
    fn test_normalize_empty_duration() {
        let mut test_span = new_test_span();
        test_span.duration = 0;

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(test_span.duration, 0);
    }

    #[test]
    fn test_normalize_negative_duration() {
        let mut test_span = new_test_span();
        test_span.duration = -50;

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(test_span.duration, 0);
    }

    #[test]
    fn test_normalize_large_duration() {
        let mut test_span = new_test_span();
        test_span.duration = i64::MAX;

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(test_span.duration, 0);
    }

    #[test]
    fn test_normalize_error_passes() {
        let mut test_span = new_test_span();
        let before_error = test_span.error;

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(before_error, test_span.error);
    }

    #[test]
    fn test_normalize_metrics_passes() {
        let mut test_span = new_test_span();
        let before_metrics = test_span.metrics.clone();

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(before_metrics, test_span.metrics);
    }

    #[test]
    fn test_normalize_meta_passes() {
        let mut test_span = new_test_span();
        let before_meta = test_span.meta.clone();

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(before_meta, test_span.meta);
    }

    #[test]
    fn test_normalize_parent_id_passes() {
        let mut test_span = new_test_span();
        let before_parent_id = test_span.parent_id;

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(before_parent_id, test_span.parent_id);
    }

    #[test]
    fn test_normalize_type_passes() {
        let mut test_span = new_test_span();
        let before_type = test_span.r#type.clone();

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(before_type, test_span.r#type);
    }

    #[test]
    fn test_normalize_type_too_long() {
        let mut test_span = new_test_span();
        test_span.r#type = "sql".repeat(1000);

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(test_span.r#type.len(), MAX_TYPE_LEN);
    }

    #[test]
    fn test_normalize_service_tag() {
        let mut test_span = new_test_span();
        test_span.service = "retargeting(api-Staging ".to_string();

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(test_span.service, "retargeting_api-staging");
    }

    #[test]
    fn test_normalize_env() {
        let mut test_span = new_test_span();
        test_span
            .meta
            .insert("env".to_string(), "DEVELOPMENT".to_string());

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!("development", test_span.meta.get("env").unwrap());
    }

    #[test]
    fn test_special_zipkin_root_span() {
        let mut test_span = new_test_span();
        test_span.parent_id = 42;
        test_span.trace_id = 42;
        test_span.span_id = 42;

        let before_trace_id = test_span.trace_id;
        let before_span_id = test_span.span_id;

        assert!(normalizer::normalize_span(&mut test_span).is_ok());
        assert_eq!(test_span.parent_id, 0);
        assert_eq!(test_span.trace_id, before_trace_id);
        assert_eq!(test_span.span_id, before_span_id);
    }

    #[test]
    fn test_normalize_trace_empty() {
        let mut trace = vec![];
        let result = normalizer::normalize_trace(&mut trace);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Normalize Trace Error: Trace is empty"));
    }

    #[test]
    fn test_normalize_trace_trace_id_mismatch() {
        let mut span_1 = new_test_span();
        let mut span_2 = new_test_span();
        span_1.trace_id = 1;
        span_2.trace_id = 2;

        let mut trace = vec![span_1, span_2];
        let result = normalizer::normalize_trace(&mut trace);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Normalize Trace Error: Trace has foreign span"));
    }

    #[test]
    fn test_normalize_trace_invalid_span_name() {
        let span_1 = new_test_span();
        let mut span_2 = new_test_span();
        span_2.name = "".to_string(); // will be normalized

        let mut trace = vec![span_1, span_2];
        assert!(normalizer::normalize_trace(&mut trace).is_ok());
        assert_eq!(trace[1].name, DEFAULT_SPAN_NAME);
    }

    #[test]
    fn test_normalize_trace() {
        let span_1 = new_test_span();
        let mut span_2 = new_test_span();
        span_2.span_id += 1;

        let mut trace = vec![span_1, span_2];
        assert!(normalizer::normalize_trace(&mut trace).is_ok());
    }

    #[test]
    fn test_is_valid_status_code() {
        assert!(normalizer::is_valid_http_status_code("100"));
        assert!(normalizer::is_valid_http_status_code("599"));
        assert!(!normalizer::is_valid_http_status_code("99"));
        assert!(!normalizer::is_valid_http_status_code("600"));
        assert!(!normalizer::is_valid_http_status_code(
            "Invalid status code"
        ));
    }

    #[test]
    fn test_normalize_chunk_populating_origin() {
        let mut root = new_test_span();
        root.meta
            .insert(normalizer::TAG_ORIGIN.to_string(), "rum".to_string());

        let mut chunk = new_test_chunk_with_span(root);
        chunk.origin = "".to_string();
        assert!(normalizer::normalize_chunk(&mut chunk, 0).is_ok());
        assert_eq!("rum".to_string(), chunk.origin);
    }

    #[test]
    fn test_normalize_chunk_not_populating_origin() {
        let mut root = new_test_span();
        root.meta
            .insert(normalizer::TAG_ORIGIN.to_string(), "rum".to_string());

        let mut chunk = new_test_chunk_with_span(root);
        chunk.origin = "lambda".to_string();
        assert!(normalizer::normalize_chunk(&mut chunk, 0).is_ok());
        assert_eq!("lambda".to_string(), chunk.origin);
    }

    #[test]
    fn test_normalize_chunk_populating_sampling_priority() {
        let mut root = new_test_span();
        root.metrics.insert(
            normalizer::TAG_SAMPLING_PRIORITY.to_string(),
            normalizer::SamplerPriority::UserKeep as i32 as f64,
        );

        let mut chunk = new_test_chunk_with_span(root);
        chunk.priority = normalizer::SamplerPriority::None as i32;
        assert!(normalizer::normalize_chunk(&mut chunk, 0).is_ok());
        assert_eq!(normalizer::SamplerPriority::UserKeep as i32, chunk.priority);
    }

    #[test]
    fn test_normalize_chunk_not_populating_sampling_priority() {
        let mut root = new_test_span();
        root.metrics.insert(
            normalizer::TAG_SAMPLING_PRIORITY.to_string(),
            normalizer::SamplerPriority::UserKeep as i32 as f64,
        );

        let mut chunk = new_test_chunk_with_span(root);
        chunk.priority = normalizer::SamplerPriority::AutoDrop as i32;
        assert!(normalizer::normalize_chunk(&mut chunk, 0).is_ok());
        assert_eq!(normalizer::SamplerPriority::AutoDrop as i32, chunk.priority);
    }

    #[test]
    fn test_normalize_chunk_invalid_root_span() {
        let mut chunk = new_test_chunk_with_span(new_test_span());

        let result = normalizer::normalize_chunk(&mut chunk, 1);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Normalize Chunk Error: root_span_index > length of trace chunk spans"
        );
    }

    #[test]
    fn test_normalize_populate_priority_from_any_span() {
        let mut chunk = new_test_chunk_with_span(new_test_span());
        chunk.priority = normalizer::SamplerPriority::None as i32;
        chunk.spans = vec![new_test_span(), new_test_span(), new_test_span()];
        chunk.spans[1].metrics.insert(
            normalizer::TAG_SAMPLING_PRIORITY.to_string(),
            normalizer::SamplerPriority::UserKeep as i32 as f64,
        );
        assert!(normalizer::normalize_chunk(&mut chunk, 0).is_ok());
        assert_eq!(normalizer::SamplerPriority::UserKeep as i32, chunk.priority);
    }
}

#[cfg(test)]
mod v04_tests {
    use crate::normalizer;
    use libdd_trace_types::span::v04::Span;
    use libdd_trace_types::span::vec_map::VecMap;
    use libdd_trace_types::span::{BytesData, SpanText};
    use std::borrow::Borrow;

    fn new_v04_test_span() -> Span<BytesData> {
        Span {
            service: <BytesData as libdd_trace_types::span::TraceData>::Text::from_owned(
                "django".to_string(),
            ),
            name: <BytesData as libdd_trace_types::span::TraceData>::Text::from_owned(
                "django.controller".to_string(),
            ),
            resource: <BytesData as libdd_trace_types::span::TraceData>::Text::from_owned(
                "GET /api".to_string(),
            ),
            r#type: <BytesData as libdd_trace_types::span::TraceData>::Text::from_owned(
                "http".to_string(),
            ),
            trace_id: 424242u128,
            span_id: 999u64,
            parent_id: 111u64,
            start: 1448466874000000000i64,
            duration: 10000000i64,
            error: 0,
            meta: VecMap::new(),
            metrics: VecMap::new(),
            meta_struct: VecMap::new(),
            span_links: vec![],
            span_events: vec![],
        }
    }

    #[test]
    fn test_normalize_trace_v04_empty() {
        let mut trace: Vec<Span<BytesData>> = vec![];
        let result = normalizer::normalize_trace_v04(&mut trace);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Trace is empty"));
    }

    #[test]
    fn test_normalize_trace_v04_zero_trace_id() {
        let mut span = new_v04_test_span();
        span.trace_id = 0;
        let mut trace = vec![span];
        let result = normalizer::normalize_trace_v04(&mut trace);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("trace_id_zero"));
    }

    #[test]
    fn test_normalize_trace_v04_zero_span_id() {
        let mut span = new_v04_test_span();
        span.span_id = 0;
        let mut trace = vec![span];
        let result = normalizer::normalize_trace_v04(&mut trace);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("span_id_zero"));
    }

    #[test]
    fn test_normalize_trace_v04_foreign_span() {
        let mut span1 = new_v04_test_span();
        let mut span2 = new_v04_test_span();
        span1.trace_id = 100u128;
        span2.trace_id = 200u128;
        let mut trace = vec![span1, span2];
        let result = normalizer::normalize_trace_v04(&mut trace);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("foreign span"));
    }

    #[test]
    fn test_normalize_trace_v04_normalizes_service() {
        let mut span = new_v04_test_span();
        span.service = <BytesData as libdd_trace_types::span::TraceData>::Text::from_owned(
            "BAD$SERVICE".to_string(),
        );
        let mut trace = vec![span];
        assert!(normalizer::normalize_trace_v04(&mut trace).is_ok());
        assert_eq!(<_>::borrow(&trace[0].service) as &str, "bad_service");
    }

    #[test]
    fn test_normalize_trace_v04_normalizes_name() {
        let mut span = new_v04_test_span();
        span.name =
            <BytesData as libdd_trace_types::span::TraceData>::Text::from_owned("".to_string());
        let mut trace = vec![span];
        assert!(normalizer::normalize_trace_v04(&mut trace).is_ok());
        assert_eq!(<_>::borrow(&trace[0].name) as &str, "unnamed_operation");
    }

    #[test]
    fn test_normalize_trace_v04_normalizes_resource() {
        let mut span = new_v04_test_span();
        span.resource =
            <BytesData as libdd_trace_types::span::TraceData>::Text::from_owned("".to_string());
        let mut trace = vec![span];
        assert!(normalizer::normalize_trace_v04(&mut trace).is_ok());
        assert_eq!(<_>::borrow(&trace[0].resource) as &str, "django.controller");
    }

    #[test]
    fn test_normalize_trace_v04_normalizes_type() {
        let mut span = new_v04_test_span();
        span.r#type =
            <BytesData as libdd_trace_types::span::TraceData>::Text::from_owned("web".repeat(50));
        let mut trace = vec![span];
        assert!(normalizer::normalize_trace_v04(&mut trace).is_ok());
        assert!((<_>::borrow(&trace[0].r#type) as &str).len() <= 100); // MAX_TYPE_LEN
    }

    #[test]
    fn test_normalize_trace_v04_zipkin_root_span() {
        let mut span = new_v04_test_span();
        span.trace_id = 42u128;
        span.span_id = 42u64;
        span.parent_id = 42u64;
        let mut trace = vec![span];
        assert!(normalizer::normalize_trace_v04(&mut trace).is_ok());
        assert_eq!(trace[0].parent_id, 0);
    }

    #[test]
    fn test_normalize_trace_v04_preserves_parent_id_not_zipkin() {
        let mut span = new_v04_test_span();
        span.parent_id = 123u64;
        let before_parent = span.parent_id;
        let mut trace = vec![span];
        assert!(normalizer::normalize_trace_v04(&mut trace).is_ok());
        assert_eq!(trace[0].parent_id, before_parent);
    }

    #[test]
    fn test_normalize_trace_v04_high_64_bit_trace_id_not_parent_id() {
        let mut span = new_v04_test_span();
        let trace_id = (1u128 << 64) | 1;
        span.trace_id = trace_id;
        span.span_id = 1u64;
        span.parent_id = 1u64;
        let mut trace = vec![span];
        assert!(normalizer::normalize_trace_v04(&mut trace).is_ok());
        assert_eq!(trace[0].parent_id, 1);
    }

    #[test]
    fn test_normalize_trace_v04_removes_invalid_http_status_code() {
        let mut span = new_v04_test_span();
        span.meta.insert(
            <BytesData as libdd_trace_types::span::TraceData>::Text::from_static_str(
                "http.status_code",
            ),
            <BytesData as libdd_trace_types::span::TraceData>::Text::from_owned("999".to_string()),
        );
        let mut trace = vec![span];
        assert!(normalizer::normalize_trace_v04(&mut trace).is_ok());
        assert!(!trace[0]
            .meta
            .iter()
            .any(|(k, _)| <_>::borrow(k) as &str == "http.status_code"));
    }

    #[test]
    fn test_normalize_trace_v04_preserves_valid_http_status_code() {
        let mut span = new_v04_test_span();
        span.meta.insert(
            <BytesData as libdd_trace_types::span::TraceData>::Text::from_static_str(
                "http.status_code",
            ),
            <BytesData as libdd_trace_types::span::TraceData>::Text::from_owned("200".to_string()),
        );
        let mut trace = vec![span];
        assert!(normalizer::normalize_trace_v04(&mut trace).is_ok());
        assert!(trace[0]
            .meta
            .iter()
            .any(|(k, _)| <_>::borrow(k) as &str == "http.status_code"));
    }

    #[test]
    fn test_normalize_trace_v04_invalid_service_not_mutated_on_validation_error() {
        let mut span = new_v04_test_span();
        span.trace_id = 0; // Trigger validation error: zero trace_id
        let invalid_service = "BAD$SERVICE";
        span.service = <BytesData as libdd_trace_types::span::TraceData>::Text::from_owned(
            invalid_service.to_string(),
        );
        let mut trace = vec![span];
        let result = normalizer::normalize_trace_v04(&mut trace);
        assert!(result.is_err());
        // Verify service was NOT normalized because validation failed before mutation
        assert_eq!(
            (<_>::borrow(&trace[0].service) as &str).to_string(),
            invalid_service
        );
    }

    #[test]
    fn test_normalize_trace_v04_normalizes_env_tag() {
        let mut span = new_v04_test_span();
        span.meta.insert(
            <BytesData as libdd_trace_types::span::TraceData>::Text::from_static_str("env"),
            <BytesData as libdd_trace_types::span::TraceData>::Text::from_owned(
                "PRODUCTION".to_string(),
            ),
        );
        let mut trace = vec![span];
        assert!(normalizer::normalize_trace_v04(&mut trace).is_ok());
        let env_val = trace[0]
            .meta
            .iter()
            .find(|(k, _)| <_>::borrow(k) as &str == "env")
            .map(|(_, v)| (<_>::borrow(v) as &str).to_string());
        assert_eq!(env_val, Some("production".to_string()));
    }

    #[test]
    fn test_normalize_trace_v04_does_not_mutate_when_already_normalized() {
        let span = new_v04_test_span();
        let original_service = (<_>::borrow(&span.service) as &str).to_string();
        let original_name = (<_>::borrow(&span.name) as &str).to_string();
        let mut trace = vec![span];
        assert!(normalizer::normalize_trace_v04(&mut trace).is_ok());
        assert_eq!(
            (<_>::borrow(&trace[0].service) as &str).to_string(),
            original_service
        );
        assert_eq!(
            (<_>::borrow(&trace[0].name) as &str).to_string(),
            original_name
        );
    }

    #[test]
    fn test_normalize_trace_v04_multiple_spans_same_trace() {
        let mut span1 = new_v04_test_span();
        let mut span2 = new_v04_test_span();
        span1.span_id = 1u64;
        span2.span_id = 2u64;
        span1.service = <BytesData as libdd_trace_types::span::TraceData>::Text::from_owned(
            "BAD_SVC1".to_string(),
        );
        span2.service = <BytesData as libdd_trace_types::span::TraceData>::Text::from_owned(
            "BAD_SVC2".to_string(),
        );
        let mut trace = vec![span1, span2];
        assert!(normalizer::normalize_trace_v04(&mut trace).is_ok());
        assert_eq!(<_>::borrow(&trace[0].service) as &str, "bad_svc1");
        assert_eq!(<_>::borrow(&trace[1].service) as &str, "bad_svc2");
    }
}
