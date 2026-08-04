// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! JSON "log exporter" trace encoder.
//!
//! Emits traces in the newline-delimited JSON format consumed by the Datadog
//! Forwarder Lambda (the legacy serverless path used when no Datadog Agent /
//! Lambda Extension is reachable). Each emitted line is a self-contained JSON
//! document of the form:
//!
//! ```text
//! {"traces":[[ {span}, {span}, ... ]]}\n
//! ```
//!
//! Spans are greedily packed into size-bounded lines. A single span that alone
//! exceeds the line cap is dropped (and counted in [`EncodeStats::spans_dropped`])
//! rather than emitted as a truncated, unparseable line.
//!
//! See the cross-language specification for the wire contract and the
//! Forwarder's `is_trace` detection requirements.

mod span;

use crate::span::v04::Span;
use crate::span::TraceData;
use span::LogSpan;
use std::io::Write;

/// Opening bytes of every emitted line: `{"traces":[[`.
const TRACE_PREFIX: &[u8] = b"{\"traces\":[[";
/// Closing bytes of every emitted line: `]]}` plus the terminating newline.
const TRACE_SUFFIX: &[u8] = b"]]}\n";
/// Fixed per-line overhead contributed by [`TRACE_PREFIX`] and [`TRACE_SUFFIX`].
const TRACE_FORMAT_OVERHEAD: usize = TRACE_PREFIX.len() + TRACE_SUFFIX.len();

/// Statistics reported by [`encode_traces`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EncodeStats {
    /// Number of spans successfully written to `out`.
    pub spans_written: usize,
    /// Number of spans dropped because a single span exceeded `max_line_size`.
    pub spans_dropped: usize,
}

/// Encodes `traces` into newline-delimited JSON "log exporter" lines, writing
/// them to `out`.
///
/// All spans across all input traces are flattened and greedily packed into
/// lines no larger than `max_line_size` bytes (including the `{"traces":[[` /
/// `]]}\n` framing). Each line contains a single inner trace array, matching the
/// reference dd-trace-js exporter. A span whose own serialized size plus the
/// framing overhead exceeds `max_line_size` is dropped (counted in
/// [`EncodeStats::spans_dropped`]) and never split across lines.
///
/// The caller is responsible for flushing `out` (e.g. stdout) after this returns.
///
/// # Errors
///
/// Returns any [`std::io::Error`] produced while writing to `out`.
///
/// # Examples
///
/// ```
/// use libdd_trace_utils::json_log_encoder::encode_traces;
/// use libdd_trace_utils::span::v04::SpanSlice;
///
/// let span = SpanSlice {
///     service: "my-fn".into(),
///     name: "aws.lambda".into(),
///     resource: "my-fn".into(),
///     trace_id: 1,
///     span_id: 2,
///     ..Default::default()
/// };
/// let traces = vec![vec![span]];
///
/// let mut out = Vec::new();
/// let stats = encode_traces(&traces, &mut out, 64 * 1024).unwrap();
///
/// assert_eq!(stats.spans_written, 1);
/// assert!(out.ends_with(b"\n"));
/// ```
pub fn encode_traces<T: TraceData>(
    traces: &[Vec<Span<T>>],
    out: &mut impl Write,
    max_line_size: usize,
) -> std::io::Result<EncodeStats> {
    let mut stats = EncodeStats::default();

    // Reusable buffers: `span_buf` holds the JSON for the span currently being
    // considered; `line` accumulates the complete current line. It is primed
    // with `TRACE_PREFIX` so that a single `write_all` per line can emit
    // prefix + spans + suffix in one syscall (see `flush_line`).
    let mut span_buf: Vec<u8> = Vec::with_capacity(512);
    let mut line: Vec<u8> = Vec::with_capacity(max_line_size.min(64 * 1024));
    line.extend_from_slice(TRACE_PREFIX);
    let mut line_span_count: usize = 0;

    for trace in traces {
        for span in trace {
            span_buf.clear();
            serde_json::to_writer(&mut span_buf, &LogSpan(span)).map_err(std::io::Error::other)?;
            let span_len = span_buf.len();

            // A span that cannot fit on a line by itself is dropped rather than
            // emitted as a truncated, unparseable line.
            if span_len + TRACE_FORMAT_OVERHEAD > max_line_size {
                stats.spans_dropped += 1;
                tracing::debug!(
                    span_len,
                    max_line_size,
                    "Span too large to send to logs, dropping"
                );
                continue;
            }

            // Flush the current line if appending this span would overflow.
            // `line` already contains `TRACE_PREFIX`; the emitted line will also
            // gain `TRACE_SUFFIX`, so account for both here.
            let comma = usize::from(line_span_count > 0);
            if line_span_count > 0
                && line.len() + comma + span_len + TRACE_SUFFIX.len() > max_line_size
            {
                flush_line(out, &mut line)?;
                line_span_count = 0;
            }

            if line_span_count > 0 {
                line.push(b',');
            }
            line.extend_from_slice(&span_buf);
            line_span_count += 1;
            stats.spans_written += 1;
        }
    }

    if line_span_count > 0 {
        flush_line(out, &mut line)?;
    }

    Ok(stats)
}

/// Writes one complete line (`{"traces":[[` + joined spans + `]]}\n`) in a single
/// `write_all`, then resets the line buffer (re-primed with `TRACE_PREFIX`) for
/// reuse.
///
/// `line` is expected to already contain `TRACE_PREFIX` followed by the
/// comma-joined spans; this appends `TRACE_SUFFIX` to complete the line.
fn flush_line(out: &mut impl Write, line: &mut Vec<u8>) -> std::io::Result<()> {
    line.extend_from_slice(TRACE_SUFFIX);
    out.write_all(line)?;
    line.clear();
    line.extend_from_slice(TRACE_PREFIX);
    Ok(())
}

#[cfg(test)]
// `SpanSlice` fields are `Cow<'a, str>` (SliceData::Text), so the `"literal".into()`
// conversions below are genuine `&str -> Cow` conversions required to compile. clippy on
// the CI target nonetheless reports them as `useless_conversion` to `&str` (a false positive
// not reproduced on all hosts); allow it here rather than dropping the necessary `.into()`.
#[allow(clippy::useless_conversion)]
mod tests {
    use super::*;
    use crate::span::v04::SpanSlice;
    use serde_json::Value;

    const MAX: usize = 64 * 1024;

    fn lines(out: &[u8]) -> Vec<String> {
        String::from_utf8(out.to_vec())
            .unwrap()
            .lines()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn golden_known_span() {
        let span = SpanSlice {
            service: "my-fn".into(),
            name: "aws.lambda".into(),
            resource: "my-fn".into(),
            r#type: "serverless".into(),
            trace_id: 1,
            span_id: 2,
            parent_id: 0,
            start: 1717200000000000000,
            duration: 1500000,
            error: 0,
            meta: [("env".into(), "prod".into())].into_iter().collect(),
            metrics: [("_sampling_priority_v1".into(), 1.0)]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let mut out = Vec::new();
        let stats = encode_traces(&[vec![span]], &mut out, MAX).unwrap();
        assert_eq!(stats.spans_written, 1);
        assert_eq!(stats.spans_dropped, 0);

        let expected = concat!(
            "{\"traces\":[[",
            "{\"trace_id\":\"0000000000000001\",",
            "\"span_id\":\"0000000000000002\",",
            "\"parent_id\":\"0000000000000000\",",
            "\"service\":\"my-fn\",",
            "\"name\":\"aws.lambda\",",
            "\"resource\":\"my-fn\",",
            "\"type\":\"serverless\",",
            "\"error\":0,",
            "\"start\":1717200000000000000,",
            "\"duration\":1500000,",
            "\"meta\":{\"env\":\"prod\"},",
            "\"metrics\":{\"_sampling_priority_v1\":1.0}",
            "}]]}\n",
        );
        assert_eq!(String::from_utf8(out).unwrap(), expected);
    }

    #[test]
    fn meta_struct_is_omitted() {
        let span = SpanSlice {
            trace_id: 1,
            span_id: 2,
            meta_struct: [("_dd.appsec.json".into(), [0x81u8, 0xa4].as_slice())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let mut out = Vec::new();
        encode_traces(&[vec![span]], &mut out, MAX).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            !text.contains("meta_struct"),
            "meta_struct must not be emitted: {text}"
        );
        // Still a valid, Forwarder-parseable line.
        let v: Value = serde_json::from_str(text.trim_end()).unwrap();
        assert!(v["traces"][0][0]["trace_id"].is_string());
    }

    #[test]
    fn hex_high_bits_and_root_parent() {
        // trace_id with the high 64 bits set => 32 hex chars.
        let trace_id: u128 = (0xABCDu128 << 64) | 0x1;
        let span = SpanSlice {
            trace_id,
            span_id: 0xFF,
            parent_id: 0,
            ..Default::default()
        };
        let mut out = Vec::new();
        encode_traces(&[vec![span]], &mut out, MAX).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("\"trace_id\":\"000000000000abcd0000000000000001\""),
            "got: {text}"
        );
        assert!(text.contains("\"span_id\":\"00000000000000ff\""));
        assert!(text.contains("\"parent_id\":\"0000000000000000\""));
    }

    #[test]
    fn error_is_integer() {
        let span = SpanSlice {
            error: 1,
            ..Default::default()
        };
        let mut out = Vec::new();
        encode_traces(&[vec![span]], &mut out, MAX).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\"error\":1,"), "got: {text}");
    }

    #[test]
    fn empty_maps_and_type_omitted() {
        let span = SpanSlice {
            name: "op".into(),
            ..Default::default()
        };
        let mut out = Vec::new();
        encode_traces(&[vec![span]], &mut out, MAX).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains("\"meta\""), "got: {text}");
        assert!(!text.contains("\"metrics\""));
        assert!(!text.contains("\"meta_struct\""));
        assert!(!text.contains("\"span_links\""));
        assert!(!text.contains("\"span_events\""));
        assert!(!text.contains("\"type\""));
    }

    #[test]
    fn string_escaping() {
        let span = SpanSlice {
            name: "say \"hi\"\n".into(),
            ..Default::default()
        };
        let mut out = Vec::new();
        encode_traces(&[vec![span]], &mut out, MAX).unwrap();
        // Each line must remain valid JSON despite the embedded quote/newline.
        for line in lines(&out) {
            let parsed: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(
                parsed["traces"][0][0]["name"].as_str().unwrap(),
                "say \"hi\"\n"
            );
        }
    }

    #[test]
    fn size_cap_batches_into_multiple_lines() {
        // Build several spans that individually fit but together exceed a small cap.
        let make = |id: u64| SpanSlice {
            name: "x".into(),
            span_id: id,
            ..Default::default()
        };
        let trace: Vec<SpanSlice> = (1..=6).map(make).collect();

        // Determine one span's serialized length to size the cap so ~2 fit per line.
        let mut one = Vec::new();
        encode_traces(&[vec![make(1)]], &mut one, MAX).unwrap();
        // `one` = prefix + span + suffix. The bare span length:
        let span_len = one.len() - TRACE_FORMAT_OVERHEAD;
        // Cap fits two spans + a comma but not three.
        let cap = TRACE_FORMAT_OVERHEAD + span_len * 2 + 1;

        let mut out = Vec::new();
        let stats = encode_traces(&[trace], &mut out, cap).unwrap();
        assert_eq!(stats.spans_written, 6);
        assert_eq!(stats.spans_dropped, 0);

        let emitted = lines(&out);
        assert_eq!(emitted.len(), 3, "expected 3 lines, got {emitted:?}");
        for line in &emitted {
            assert!(line.len() <= cap, "line over cap: {} > {cap}", line.len());
            let parsed: Value = serde_json::from_str(line).unwrap();
            assert_eq!(parsed["traces"][0].as_array().unwrap().len(), 2);
        }
    }

    #[test]
    fn oversize_single_span_dropped() {
        let big = "a".repeat(10_000);
        let span = SpanSlice {
            name: big.as_str().into(),
            span_id: 1,
            ..Default::default()
        };
        let small = SpanSlice {
            name: "ok".into(),
            span_id: 2,
            ..Default::default()
        };
        let mut out = Vec::new();
        // Cap large enough for `small` but far too small for `big`.
        let stats = encode_traces(&[vec![span, small]], &mut out, 1024).unwrap();
        assert_eq!(stats.spans_dropped, 1);
        assert_eq!(stats.spans_written, 1);

        let emitted = lines(&out);
        assert_eq!(emitted.len(), 1);
        let parsed: Value = serde_json::from_str(&emitted[0]).unwrap();
        assert_eq!(parsed["traces"][0][0]["name"].as_str().unwrap(), "ok");
    }

    #[test]
    fn metric_non_finite_serializes_null() {
        // serde_json renders non-finite f64 as JSON null; the line must remain
        // parseable and the metric values must be null (not NaN/Infinity tokens).
        let span = SpanSlice {
            span_id: 1,
            metrics: [("nan".into(), f64::NAN), ("inf".into(), f64::INFINITY)]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let mut out = Vec::new();
        encode_traces(&[vec![span]], &mut out, MAX).unwrap();
        let emitted = lines(&out);
        assert_eq!(emitted.len(), 1);
        let parsed: Value = serde_json::from_str(&emitted[0]).unwrap();
        let metrics = &parsed["traces"][0][0]["metrics"];
        assert!(metrics["nan"].is_null(), "got: {metrics}");
        assert!(metrics["inf"].is_null(), "got: {metrics}");
    }

    #[test]
    fn multi_trace_flattened_into_one_line() {
        // Two input traces, both well under the cap, flatten into a single line
        // whose single inner trace array holds both spans.
        let span_a = SpanSlice {
            name: "a".into(),
            span_id: 1,
            ..Default::default()
        };
        let span_b = SpanSlice {
            name: "b".into(),
            span_id: 2,
            ..Default::default()
        };
        let mut out = Vec::new();
        let stats = encode_traces(&[vec![span_a], vec![span_b]], &mut out, MAX).unwrap();
        assert_eq!(stats.spans_written, 2);
        let emitted = lines(&out);
        assert_eq!(emitted.len(), 1, "expected one line, got {emitted:?}");
        let parsed: Value = serde_json::from_str(&emitted[0]).unwrap();
        let inner = parsed["traces"][0].as_array().unwrap();
        assert_eq!(inner.len(), 2);
        assert_eq!(parsed["traces"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn span_links_and_events_emitted_when_present() {
        use crate::span::v04::{SpanEvent, SpanLink};

        let span = SpanSlice {
            span_id: 1,
            span_links: vec![SpanLink {
                trace_id: 7,
                span_id: 8,
                ..Default::default()
            }],
            span_events: vec![SpanEvent {
                time_unix_nano: 123,
                name: "evt".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut out = Vec::new();
        encode_traces(&[vec![span]], &mut out, MAX).unwrap();
        let emitted = lines(&out);
        assert_eq!(emitted.len(), 1);
        let parsed: Value = serde_json::from_str(&emitted[0]).unwrap();
        let span_json = &parsed["traces"][0][0];
        // Lock the inner wire shape (field names/values), not just presence.
        assert_eq!(span_json["span_links"][0]["trace_id"], 7);
        assert_eq!(span_json["span_links"][0]["span_id"], 8);
        assert_eq!(span_json["span_events"][0]["name"], "evt");
        assert_eq!(span_json["span_events"][0]["time_unix_nano"], 123);
    }

    #[test]
    fn empty_inner_trace_writes_nothing() {
        // One trace containing zero spans: nothing is emitted and no spans counted.
        let traces: Vec<Vec<SpanSlice>> = vec![vec![]];
        let mut out = Vec::new();
        let stats = encode_traces(&traces, &mut out, MAX).unwrap();
        assert!(out.is_empty());
        assert_eq!(stats.spans_written, 0);
        assert_eq!(stats.spans_dropped, 0);
    }

    #[test]
    fn empty_input_writes_nothing() {
        let traces: Vec<Vec<SpanSlice>> = vec![];
        let mut out = Vec::new();
        let stats = encode_traces(&traces, &mut out, MAX).unwrap();
        assert_eq!(stats, EncodeStats::default());
        assert!(out.is_empty());
    }

    // Mirrors the Datadog Forwarder `is_trace` detection contract: every line
    // parses as JSON, top-level `traces` is a non-empty array whose `[0]` is a
    // non-empty array whose `[0]` has a non-null string `trace_id`, and every
    // line ends with `\n`.
    #[test]
    fn forwarder_is_trace_contract() {
        let trace: Vec<SpanSlice> = (1u64..=5)
            .map(|i| SpanSlice {
                service: "svc".into(),
                name: "op".into(),
                trace_id: u128::from(i),
                span_id: i + 100,
                ..Default::default()
            })
            .collect();

        let mut out = Vec::new();
        encode_traces(&[trace], &mut out, 200).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(!text.is_empty());
        // Every line is newline-terminated.
        for chunk in text.split_inclusive('\n') {
            assert!(chunk.ends_with('\n'), "line not newline-terminated");
            let line = chunk.trim_end_matches('\n');
            let parsed: Value = serde_json::from_str(line).unwrap();

            let traces = parsed.get("traces").and_then(Value::as_array).unwrap();
            assert!(!traces.is_empty());
            let first = traces[0].as_array().unwrap();
            assert!(!first.is_empty());
            let trace_id = first[0].get("trace_id").unwrap();
            assert!(trace_id.is_string());
            assert!(!trace_id.as_str().unwrap().is_empty());
        }
    }
}
