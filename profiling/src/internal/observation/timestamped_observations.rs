// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Used to store timestamped observations in a compressed buffer. Assumption is that we don't need
//! this data until
// serialization, so it's better to pack it in while we're holding it.

use super::super::LabelSetId;
use super::super::Sample;
use super::super::StackTraceId;
use crate::collections::identifiable::Id;
use crate::internal::Timestamp;
use byteorder::{NativeEndian, ReadBytesExt};
use lz4_flex::frame::FrameDecoder;
use lz4_flex::frame::FrameEncoder;
use std::io::Cursor;
use std::io::Write;

#[derive(Debug)]
pub struct TimestampedObservations {
    compressed_timestamped_data: FrameEncoder<Vec<u8>>,
    sample_types_len: usize,
}

impl TimestampedObservations {
    // As documented in the internal Datadog doc "Ruby timeline memory fragmentation impact
    // investigation", allowing the timeline storage vec to slowly expand creates A LOT of
    // memory fragmentation for apps that employ multiple threads.
    // To avoid this, we've picked a default buffer size of 1MB that very rarely needs to grow, and
    // when it does, is expected to grow in larger steps.
    const DEFAULT_BUFFER_SIZE: usize = 1_048_576;

    pub fn new(sample_types_len: usize) -> Self {
        Self {
            compressed_timestamped_data: FrameEncoder::new(Vec::with_capacity(
                Self::DEFAULT_BUFFER_SIZE,
            )),
            sample_types_len,
        }
    }

    pub fn with_no_backing_store() -> Self {
        Self {
            compressed_timestamped_data: FrameEncoder::new(vec![]),
            sample_types_len: 0,
        }
    }

    pub fn add(&mut self, sample: Sample, ts: Timestamp, values: &[i64]) -> anyhow::Result<()> {
        // We explicitly turn the data into a stream of bytes, feeding it to the compressor.
        // @ivoanjo: I played with introducing a structure to serialize it all-at-once, but it seems
        // to be a lot of boilerplate (of which cost I'm not sure) to basically do the same
        // as these few lines so in the end I came back to this.

        let stack_trace_id: u32 = sample.stacktrace.into();
        let labels_id: u32 = sample.labels.into();
        let timestamp = i64::from(ts);

        self.compressed_timestamped_data
            .write_all(&stack_trace_id.to_ne_bytes())?;
        self.compressed_timestamped_data
            .write_all(&labels_id.to_ne_bytes())?;
        self.compressed_timestamped_data
            .write_all(&timestamp.to_ne_bytes())?;

        for v in values {
            self.compressed_timestamped_data
                .write_all(&(v).to_ne_bytes())?;
        }

        Ok(())
    }

    pub fn into_iter(self) -> TimestampedObservationsIter {
        TimestampedObservationsIter {
            decoder: FrameDecoder::new(Cursor::new(
                self.compressed_timestamped_data.finish().unwrap(),
            )),
            sample_types_len: self.sample_types_len,
        }
    }
}

pub struct TimestampedObservationsIter {
    decoder: FrameDecoder<Cursor<Vec<u8>>>,
    sample_types_len: usize,
}

impl Iterator for TimestampedObservationsIter {
    type Item = (Sample, Timestamp, Vec<i64>);

    fn next(&mut self) -> Option<Self::Item> {
        // In here we read the bytes in the same order as in add above

        let stacktrace = self.decoder.read_u32::<NativeEndian>().ok()?;
        let labels = self.decoder.read_u32::<NativeEndian>().ok()?;
        let ts = self.decoder.read_i64::<NativeEndian>().ok()?;
        let mut values = Vec::with_capacity(self.sample_types_len);
        for _ in 0..self.sample_types_len {
            values.push(self.decoder.read_i64::<NativeEndian>().ok()?);
        }
        Some((
            Sample {
                stacktrace: StackTraceId::from_offset(stacktrace as usize),
                labels: LabelSetId::from_offset(labels as usize),
            },
            std::num::NonZeroI64::new(ts)?,
            values,
        ))
    }
}
