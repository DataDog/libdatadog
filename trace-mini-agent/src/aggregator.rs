// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_trace_utils::send_data::SendData;
use std::collections::VecDeque;

/// Maximum content size per payload uncompressed in bytes,
/// that the Datadog Trace API accepts. The value is 3.2 MB.
///
/// <https://github.com/DataDog/datadog-agent/blob/9d57c10a9eeb3916e661d35dbd23c6e36395a99d/pkg/trace/writer/trace.go#L27-L31>
pub const MAX_CONTENT_SIZE_BYTES: usize = (32 * 1_024 * 1_024) / 10;

#[allow(clippy::module_name_repetitions)]
pub struct TraceAggregator {
    queue: VecDeque<SendData>,
    max_content_size_bytes: usize,
    buffer: Vec<SendData>,
}

impl Default for TraceAggregator {
    fn default() -> Self {
        TraceAggregator {
            queue: VecDeque::new(),
            max_content_size_bytes: MAX_CONTENT_SIZE_BYTES,
            buffer: Vec::new(),
        }
    }
}

impl TraceAggregator {
    #[allow(dead_code)]
    #[allow(clippy::must_use_candidate)]
    pub fn new(max_content_size_bytes: usize) -> Self {
        TraceAggregator {
            queue: VecDeque::new(),
            max_content_size_bytes,
            buffer: Vec::new(),
        }
    }

    pub fn add(&mut self, p: SendData) {
        self.queue.push_back(p);
    }

    pub fn get_batch(&mut self) -> Vec<SendData> {
        let mut batch_size = 0;

        // Fill the batch
        while batch_size < self.max_content_size_bytes {
            if let Some(payload) = self.queue.pop_front() {
                let payload_size = payload.len();

                // Put stats back in the queue
                if batch_size + payload_size > self.max_content_size_bytes {
                    self.queue.push_front(payload);
                    break;
                }
                batch_size += payload_size;
                self.buffer.push(payload);
            } else {
                break;
            }
        }

        std::mem::take(&mut self.buffer)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use datadog_trace_utils::{
        trace_utils::TracerHeaderTags, tracer_payload::TracerPayloadCollection,
    };
    use ddcommon::Endpoint;

    use super::*;

    #[test]
    fn test_add() {
        let mut aggregator = TraceAggregator::default();
        let tracer_header_tags = TracerHeaderTags {
            lang: "lang",
            lang_version: "lang_version",
            lang_interpreter: "lang_interpreter",
            lang_vendor: "lang_vendor",
            tracer_version: "tracer_version",
            container_id: "container_id",
            client_computed_top_level: true,
            client_computed_stats: true,
            dropped_p0_traces: 0,
            dropped_p0_spans: 0,
        };
        let payload = SendData::new(
            1,
            TracerPayloadCollection::V07(Vec::new()),
            tracer_header_tags,
            &Endpoint::from_slice("localhost"),
        );

        aggregator.add(payload.clone());
        assert_eq!(aggregator.queue.len(), 1);
        assert_eq!(aggregator.queue[0].is_empty(), payload.is_empty());
    }

    #[test]
    fn test_get_batch() {
        let mut aggregator = TraceAggregator::default();
        let tracer_header_tags = TracerHeaderTags {
            lang: "lang",
            lang_version: "lang_version",
            lang_interpreter: "lang_interpreter",
            lang_vendor: "lang_vendor",
            tracer_version: "tracer_version",
            container_id: "container_id",
            client_computed_top_level: true,
            client_computed_stats: true,
            dropped_p0_traces: 0,
            dropped_p0_spans: 0,
        };
        let payload = SendData::new(
            1,
            TracerPayloadCollection::V07(Vec::new()),
            tracer_header_tags,
            &Endpoint::from_slice("localhost"),
        );

        aggregator.add(payload.clone());
        assert_eq!(aggregator.queue.len(), 1);
        let batch = aggregator.get_batch();
        assert_eq!(batch.len(), 1);
    }

    #[test]
    fn test_get_batch_full_entries() {
        let mut aggregator = TraceAggregator::new(2);
        let tracer_header_tags = TracerHeaderTags {
            lang: "lang",
            lang_version: "lang_version",
            lang_interpreter: "lang_interpreter",
            lang_vendor: "lang_vendor",
            tracer_version: "tracer_version",
            container_id: "container_id",
            client_computed_top_level: true,
            client_computed_stats: true,
            dropped_p0_traces: 0,
            dropped_p0_spans: 0,
        };
        let payload = SendData::new(
            1,
            TracerPayloadCollection::V07(Vec::new()),
            tracer_header_tags,
            &Endpoint::from_slice("localhost"),
        );

        // Add 3 payloads
        aggregator.add(payload.clone());
        aggregator.add(payload.clone());
        aggregator.add(payload.clone());

        // The batch should only contain the first 2 payloads
        let first_batch = aggregator.get_batch();
        assert_eq!(first_batch.len(), 2);
        assert_eq!(aggregator.queue.len(), 1);

        // The second batch should only contain the last log
        let second_batch = aggregator.get_batch();
        assert_eq!(second_batch.len(), 1);
        assert_eq!(aggregator.queue.len(), 0);
    }
}
