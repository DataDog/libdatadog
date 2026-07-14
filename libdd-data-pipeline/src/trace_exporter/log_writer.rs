// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Stdout "log exporter" trace transport.
//!
//! Encodes trace chunks as newline-delimited JSON in the format consumed by the
//! Datadog Forwarder and writes them through the [`LogWriterCapability`] (stdout
//! on native targets; host-provided, e.g. handed to JavaScript, on wasm). This
//! is used in serverless environments (primarily AWS Lambda) where no agent is
//! reachable; a downstream tool (the Datadog Forwarder) tails the platform logs
//! and submits the traces to the trace intake.

use libdd_capabilities::LogWriterCapability;
use libdd_trace_utils::json_log_encoder::{encode_traces, EncodeStats};
use libdd_trace_utils::span::{v04::Span, TraceData};

/// Default maximum size of a single emitted log line, in bytes.
///
/// Matches the AWS CloudWatch Logs per-event limit (256 KiB), consistent with
/// dd-trace-go's `logBufferLimit`. Spans are packed greedily up to this size;
/// a single span that alone exceeds it is dropped (see [`encode_traces`]).
pub(crate) const DEFAULT_LOG_MAX_LINE_SIZE: usize = 256 * 1024;

/// Encode `traces` to newline-delimited Forwarder JSON and write them through the
/// log-output capability. Returns counts of spans written/dropped.
///
/// Writes are synchronous: on native targets this blocks on a stdout write, so
/// log-export mode is intended for single-threaded / current-thread serverless
/// runtimes (e.g. AWS Lambda) where there is no shared async reactor to stall.
pub(crate) fn write_log_traces<C: LogWriterCapability + ?Sized, T: TraceData>(
    capabilities: &C,
    traces: &[Vec<Span<T>>],
    max_line_size: usize,
) -> std::io::Result<EncodeStats> {
    let mut buf: Vec<u8> = Vec::new();
    let stats = encode_traces(traces, &mut buf, max_line_size)?;
    if !buf.is_empty() {
        capabilities.write_log_output(&buf)?;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_trace_utils::span::v04::Span;
    use libdd_trace_utils::span::SliceData;
    use std::sync::{Arc, Mutex};

    /// Test capability that captures written bytes instead of touching stdout.
    #[derive(Clone, Default)]
    struct CapturingLog(Arc<Mutex<Vec<u8>>>);

    impl LogWriterCapability for CapturingLog {
        fn write_log_output(&self, bytes: &[u8]) -> std::io::Result<()> {
            self.0
                .lock()
                .expect("capture lock")
                .extend_from_slice(bytes);
            Ok(())
        }
    }

    #[test]
    fn encodes_and_writes_through_capability() {
        let cap = CapturingLog::default();
        let traces = vec![vec![Span::<SliceData<'static>> {
            trace_id: 1,
            span_id: 2,
            ..Default::default()
        }]];

        let stats = write_log_traces(&cap, &traces, DEFAULT_LOG_MAX_LINE_SIZE).expect("write ok");
        assert_eq!(stats.spans_written, 1);
        assert_eq!(stats.spans_dropped, 0);

        let out = cap.0.lock().expect("lock");
        let text = std::str::from_utf8(&out).expect("utf8");
        assert!(text.ends_with('\n'), "line must be newline-terminated");
        let v: serde_json::Value = serde_json::from_str(text.trim_end()).expect("valid json");
        // Forwarder is_trace contract.
        assert!(v["traces"][0][0]["trace_id"].is_string());
        assert_eq!(v["traces"][0][0]["span_id"], "0000000000000002");
    }

    #[test]
    fn empty_traces_do_not_call_capability() {
        let cap = CapturingLog::default();
        let traces: Vec<Vec<Span<SliceData<'static>>>> = vec![];
        let stats = write_log_traces(&cap, &traces, DEFAULT_LOG_MAX_LINE_SIZE).expect("ok");
        assert_eq!(stats.spans_written, 0);
        assert!(cap.0.lock().expect("lock").is_empty());
    }
}
