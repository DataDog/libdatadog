// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Closing the spans of a v1 tracer payload.
//!
//! The body is decoded and encoded again rather than edited in place: v1
//! interns strings by first use, so a new attribute string would renumber
//! every later one.

use super::{Closed, CrashDetails};
use libdd_trace_utils::msgpack_decoder::v1 as decode_v1;
use libdd_trace_utils::msgpack_encoder::v1 as encode_v1;
use libdd_trace_utils::span::v1::{AttributeValue, SpanSlice};
use std::borrow::Cow;

pub(super) fn close(
    body: &[u8],
    span_ids: Option<&[u64]>,
    crash: &CrashDetails,
) -> anyhow::Result<Closed> {
    let (mut payload, consumed) =
        decode_v1::from_slice(body).map_err(|e| anyhow::anyhow!("v1 decode: {e:?}"))?;
    anyhow::ensure!(
        consumed == body.len(),
        "v1 decode: {} trailing bytes",
        body.len() - consumed
    );
    for chunk in &mut payload.chunks {
        for span in &mut chunk.spans {
            if span_ids.is_none_or(|ids| ids.contains(&span.span_id)) {
                end_at_crash(span, crash);
            }
        }
    }
    Ok(Closed {
        body: encode_v1::to_vec_from_v1(&payload),
        trace_count: payload.chunks.len(),
    })
}

fn end_at_crash<'a>(span: &mut SpanSlice<'a>, crash: &'a CrashDetails) {
    // Never shorten a span or zero it: a clock that moved back keeps the
    // duration the tracer staged.
    if let Some(end) = crash.crash_unix_ns.filter(|end| *end > span.start) {
        span.duration = span.duration.max(end - span.start);
    }
    span.error = true;
    for (key, value) in crash.attributes() {
        span.attributes.insert_or_replace(
            Cow::Borrowed(key),
            AttributeValue::String(Cow::Borrowed(value)),
        );
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::{CRASH_UUID_KEY, IS_CRASH_KEY};
    use super::*;
    use libdd_trace_utils::span::SliceData;
    use libdd_trace_utils::span::v1::{SpanKind, TraceChunk, TracerPayload};

    const START: i64 = 1_700_000_000_000_000_000;

    pub(in super::super) fn span(span_id: u64, env: &str) -> SpanSlice<'_> {
        let mut s = SpanSlice {
            service: "trainer".into(),
            name: "training.rank".into(),
            resource: "training.rank".into(),
            r#type: "custom".into(),
            span_id,
            start: START,
            duration: 5,
            span_kind: SpanKind::Internal,
            env: env.into(),
            ..Default::default()
        };
        s.attributes.insert(
            "training.status".into(),
            AttributeValue::String("failed".into()),
        );
        s.attributes.insert(
            "error.message".into(),
            AttributeValue::String("Process crashed during training".into()),
        );
        s
    }

    /// A body whose strings are interned after the span's attributes too (span
    /// env, payload env and hostname), so an in-place edit would misnumber them.
    pub(in super::super) fn body(spans: Vec<SpanSlice<'_>>) -> Vec<u8> {
        let payload: TracerPayload<SliceData<'_>> = TracerPayload {
            language_name: "c".into(),
            tracer_version: "v0.33.0".into(),
            env: "dev".into(),
            hostname: "gpu-host".into(),
            chunks: vec![TraceChunk {
                trace_id: [3; 16],
                priority: Some(1),
                spans,
                ..Default::default()
            }],
            ..Default::default()
        };
        encode_v1::to_vec_from_v1(&payload)
    }

    fn crash(at: Option<i64>) -> CrashDetails {
        CrashDetails {
            crash_unix_ns: at,
            error_type: "SIGSEGV".to_string(),
            error_message: Some("Crash during training (SIGSEGV)".to_string()),
            uuid: "abc".to_string(),
        }
    }

    fn string_attr<'s>(span: &'s SpanSlice<'_>, key: &str) -> Option<&'s str> {
        match span.attributes.get(key) {
            Some(AttributeValue::String(s)) => Some(s.as_ref()),
            _ => None,
        }
    }

    #[test]
    fn ends_spans_at_the_crash_with_its_details() {
        let crash = crash(Some(START + 42_000_000_000));
        let closed = close(&body(vec![span(7, "dev")]), None, &crash).unwrap();
        assert_eq!(closed.trace_count, 1);

        let (payload, used) = decode_v1::from_slice(&closed.body).unwrap();
        assert_eq!(used, closed.body.len());
        let span = &payload.chunks[0].spans[0];
        assert_eq!(span.duration, 42_000_000_000);
        assert!(span.error);
        assert_eq!(string_attr(span, "error.type"), Some("SIGSEGV"));
        assert_eq!(
            string_attr(span, "error.message"),
            Some("Crash during training (SIGSEGV)")
        );
        assert_eq!(string_attr(span, IS_CRASH_KEY), Some("true"));
        assert_eq!(string_attr(span, "track_error"), None);
        assert_eq!(string_attr(span, CRASH_UUID_KEY), Some("abc"));
        // The tracer's own attributes, and strings interned after the new
        // ones, keep their meaning.
        assert_eq!(string_attr(span, "training.status"), Some("failed"));
        assert_eq!(span.env, "dev");
        assert_eq!(payload.env, "dev");
        assert_eq!(payload.hostname, "gpu-host");
        assert_eq!(payload.chunks[0].trace_id, [3; 16]);
    }

    #[test]
    fn ends_only_the_listed_spans() {
        let crash = crash(Some(START + 10));
        let closed = close(
            &body(vec![span(7, "dev"), span(8, "dev")]),
            Some(&[8]),
            &crash,
        )
        .unwrap();
        let (payload, _) = decode_v1::from_slice(&closed.body).unwrap();
        let [open, ended] = &payload.chunks[0].spans[..] else {
            panic!("expected two spans");
        };
        assert!(!open.error);
        assert_eq!(open.duration, 5);
        assert_eq!(string_attr(open, IS_CRASH_KEY), None);
        assert!(ended.error);
        assert_eq!(ended.duration, 10);
    }

    #[test]
    fn keeps_the_staged_message_when_the_report_has_none() {
        let crash = CrashDetails {
            error_message: None,
            ..crash(None)
        };
        let closed = close(&body(vec![span(7, "dev")]), None, &crash).unwrap();
        let (payload, _) = decode_v1::from_slice(&closed.body).unwrap();
        let span = &payload.chunks[0].spans[0];
        assert_eq!(
            string_attr(span, "error.message"),
            Some("Process crashed during training")
        );
        assert_eq!(string_attr(span, "error.type"), Some("SIGSEGV"));
    }

    #[test]
    fn keeps_the_staged_duration_without_a_usable_crash_time() {
        for at in [None, Some(START - 1), Some(START + 1)] {
            let closed = close(&body(vec![span(7, "dev")]), None, &crash(at)).unwrap();
            let (payload, _) = decode_v1::from_slice(&closed.body).unwrap();
            let span = &payload.chunks[0].spans[0];
            assert_eq!(span.duration, 5, "crash at {at:?}");
            assert!(span.error);
        }
    }

    #[test]
    fn rejects_undecodable_bodies_and_trailing_bytes() {
        assert!(close(&[0xc1, 0x00], None, &crash(None)).is_err());
        let mut staged = body(vec![span(7, "dev")]);
        staged.push(0xc0);
        assert!(close(&staged, None, &crash(None)).is_err());
    }
}
