// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! v1 trace chunk support for the [`TraceBuffer`](super::TraceBuffer): size
//! estimation for the buffer's accounting, and the default export that sends
//! buffered v1 chunks through the trace exporter.

use std::pin::Pin;
#[cfg(feature = "test-utils")]
use std::time::Duration;

use libdd_capabilities::{HttpClientCapability, LogWriterCapability, MaybeSend, SleepCapability};
use libdd_shared_runtime::SharedRuntime;
use libdd_trace_utils::span::v1::{TraceChunkBytes, span_pool::PooledTraceChunks};

use crate::trace_buffer::{BufferChunk, BufferSpan, DefaultExport, Export};
use crate::trace_exporter::{agent_response::AgentResponse, error::TraceExporterError};

impl<T> BufferSpan for libdd_trace_utils::span::v1::Span<T>
where
    T: libdd_trace_utils::span::TraceData,
    T::Text: AsRef<str>,
    T::Bytes: AsRef<[u8]>,
{
    fn byte_size(&self) -> usize {
        // span_id(8) + parent_id(8) + start(8) + duration(8) + error(1) + span_kind(4)
        let mut size: usize = 37;

        size += self.service.as_ref().len();
        size += self.name.as_ref().len();
        size += self.resource.as_ref().len();
        size += self.r#type.as_ref().len();
        size += self.env.as_ref().len();
        size += self.version.as_ref().len();
        size += self.component.as_ref().len();

        // We expect VecMaps to be already deduped at this point, so `defensive_dedup` stays
        // cheap (and alloc-free).
        for (k, v) in self.attributes.defensive_dedup().iter() {
            size += k.as_ref().len() + v1_attribute_size(v);
        }
        for link in &self.span_links {
            // trace_id(16) + span_id(8) + flags(4) = 28
            size += 28 + link.tracestate.as_ref().len();
            for (k, v) in link.attributes.defensive_dedup().iter() {
                size += k.as_ref().len() + v1_attribute_size(v);
            }
        }
        for event in &self.span_events {
            // time_unix_nano(8)
            size += 8 + event.name.as_ref().len();
            for (k, v) in event.attributes.defensive_dedup().iter() {
                size += k.as_ref().len() + v1_attribute_size(v);
            }
        }
        size
    }
}

/// Size of a v1 attribute value, arrays and nested maps included.
fn v1_attribute_size<T>(v: &libdd_trace_utils::span::v1::AttributeValue<T>) -> usize
where
    T: libdd_trace_utils::span::TraceData,
    T::Text: AsRef<str>,
    T::Bytes: AsRef<[u8]>,
{
    use libdd_trace_utils::span::v1::AttributeValue;
    match v {
        AttributeValue::String(s) => s.as_ref().len(),
        AttributeValue::Bool(_) => 1,
        AttributeValue::Int(_) => 8,
        AttributeValue::Float(_) => 8,
        AttributeValue::Bytes(b) => b.as_ref().len(),
        AttributeValue::Array(items) => items.iter().map(v1_attribute_size).sum(),
        AttributeValue::KeyValueList(map) => map
            .defensive_dedup()
            .iter()
            .map(|(k, v)| k.as_ref().len() + v1_attribute_size(v))
            .sum(),
    }
}

impl<T> BufferChunk for libdd_trace_utils::span::v1::TraceChunk<T>
where
    T: libdd_trace_utils::span::TraceData,
    T::Text: AsRef<str>,
    T::Bytes: AsRef<[u8]>,
{
    fn byte_size(&self) -> usize {
        // trace_id(16) + priority(4) + sampling_mechanism(4) + dropped_trace(1)
        let mut size: usize = 25;
        size += self.origin.as_ref().len();
        for (k, v) in self.attributes.defensive_dedup().iter() {
            size += k.as_ref().len() + v1_attribute_size(v);
        }
        for span in &self.spans {
            size += span.byte_size();
        }
        size
    }

    fn span_count(&self) -> usize {
        self.spans.len()
    }
}

impl<C, R> Export<TraceChunkBytes> for DefaultExport<C, R>
where
    C: HttpClientCapability + SleepCapability + LogWriterCapability + MaybeSend + Sync + 'static,
    R: SharedRuntime + std::fmt::Debug + Send + Sync + 'static,
{
    fn export_trace_chunks(
        &mut self,
        trace_chunks: Vec<TraceChunkBytes>,
        force_flush: bool,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<AgentResponse, TraceExporterError>> + Send + '_,
        >,
    > {
        Box::pin(async move {
            // A force flush can arrive with no trace chunks when the buffer was empty.
            // There are no traces to send in that case, so skip the network round-trip.
            // The only work left is flushing the buffered client-side stats that the
            // force flush is meant to drain.
            let res = if trace_chunks.is_empty() {
                Ok(AgentResponse::Unchanged)
            } else {
                self.trace_exporter
                    .send_trace_chunks_async_v1(PooledTraceChunks::unpooled(trace_chunks))
                    .await
            };
            if force_flush {
                self.trace_exporter.flush_client_side_stats_async().await;
            }
            res
        })
    }

    #[cfg(feature = "test-utils")]
    fn wait_ready(
        &mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + '_>> {
        Box::pin(async {
            self.trace_exporter
                .wait_agent_info_ready(Duration::from_secs(5))
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use libdd_shared_runtime::{BlockingRuntime, ForkSafeRuntime, SharedRuntime};
    use libdd_trace_utils::span::v1::{SpanBytes as V1SpanBytes, TraceChunkBytes};

    use crate::trace_buffer::{BufferChunk, Export, TraceBuffer, TraceBufferConfig};
    use crate::trace_exporter::{agent_response::AgentResponse, error::TraceExporterError};

    /// A v1 trace chunk with `spans` default spans. The buffer accepts it as a chunk type
    /// that is not a `Vec` of spans.
    fn v1_chunk(spans: usize) -> TraceChunkBytes {
        TraceChunkBytes {
            spans: (0..spans).map(|_| V1SpanBytes::default()).collect(),
            ..Default::default()
        }
    }

    struct V1ChunkExporter(
        Box<dyn FnMut(Vec<TraceChunkBytes>, bool) + Send + Sync>,
        Arc<tokio::sync::Semaphore>,
    );

    impl std::fmt::Debug for V1ChunkExporter {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_tuple("V1ChunkExporter").finish()
        }
    }

    impl Export<TraceChunkBytes> for V1ChunkExporter {
        fn export_trace_chunks(
            &mut self,
            trace_chunks: Vec<TraceChunkBytes>,
            force_flush: bool,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<AgentResponse, TraceExporterError>>
                    + Send
                    + '_,
            >,
        > {
            (self.0)(trace_chunks, force_flush);
            self.1.add_permits(1);
            Box::pin(async { Ok(AgentResponse::Unchanged) })
        }
    }

    #[test]
    fn test_v1_trace_chunk_size_impl() {
        let empty = v1_chunk(0);
        assert!(empty.is_empty());
        assert_eq!(empty.span_count(), 0);
        // Chunk overhead only: trace_id(16) + priority(4) + sampling_mechanism(4) +
        // dropped_trace(1).
        assert_eq!(empty.byte_size(), 25);

        let one = v1_chunk(1);
        let two = v1_chunk(2);
        assert_eq!(one.span_count(), 1);
        assert_eq!(two.span_count(), 2);
        assert!(!two.is_empty());
        // Each default span adds its fixed size: span_id(8) + parent_id(8) + start(8) +
        // duration(8) + error(1) + span_kind(4).
        assert_eq!(one.byte_size(), 25 + 37);
        assert_eq!(two.byte_size(), 25 + 2 * 37);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_v1_trace_chunks_flow_through_the_buffer() {
        // A v1 chunk is not a Vec. This exercises the buffer with a chunk type that
        // is not a `Vec` of spans.
        let rt = Arc::new(ForkSafeRuntime::new().unwrap());
        let sem: Arc<tokio::sync::Semaphore> = Arc::new(tokio::sync::Semaphore::new(0));
        let (sender, worker) = TraceBuffer::new(
            TraceBufferConfig::default()
                .max_buffered_bytes(100_000)
                .flush_threshold_bytes(100_000)
                .max_flush_interval(Duration::from_secs(u32::MAX as u64)),
            Box::new(|_r: Result<AgentResponse, TraceExporterError>| {}),
            Box::new(V1ChunkExporter(
                Box::new(|chunks, force_flush| {
                    assert!(force_flush, "force_flush must drive the export");
                    let mut span_counts: Vec<_> = chunks.iter().map(|c| c.span_count()).collect();
                    span_counts.sort_unstable();
                    assert_eq!(span_counts, vec![1, 2]);
                }),
                sem.clone(),
            )),
        );
        let _ = rt.spawn_worker(worker, true).unwrap();

        // A chunk without spans is empty, so the buffer drops it at the door.
        sender.send_chunk(v1_chunk(0)).unwrap();
        sender.send_chunk(v1_chunk(1)).unwrap();
        sender.send_chunk(v1_chunk(2)).unwrap();

        assert_eq!(sem.available_permits(), 0);
        sender.force_flush().unwrap();
        let _ = rt.block_on(sem.acquire_many(1)).unwrap().unwrap();

        let metrics = sender.queue_metrics().get_metrics();
        assert_eq!(metrics.spans_queued, 3);
        assert_eq!(metrics.spans_dropped_full_buffer, 0);

        rt.shutdown(None).unwrap();
        sender.wait_close_done(Duration::from_secs(10)).unwrap();
    }
}
