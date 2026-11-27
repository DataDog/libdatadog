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
use crate::profiles::{DefaultObservationCodec as DefaultCodec, ObservationCodec};
use byteorder::{NativeEndian, ReadBytesExt};
use std::io::{self, BufWriter, Read, Write};

pub type TimestampedObservations = TimestampedObservationsImpl<DefaultCodec>;

pub struct TimestampedObservationsImpl<C: ObservationCodec> {
    compressed_timestamped_data: BufWriter<C::Encoder>,
    sample_types_len: usize,
}

pub struct TimestampedObservationsIterImpl<C: ObservationCodec> {
    decoder: C::Decoder,
    sample_types_len: usize,
}

impl<C: ObservationCodec> TimestampedObservationsImpl<C> {
    // As documented in the internal Datadog doc "Ruby timeline memory fragmentation impact
    // investigation", allowing the timeline storage vec to slowly expand creates A LOT of
    // memory fragmentation for apps that employ multiple threads.
    // To avoid this, we've picked a default buffer size of 1MB that very rarely needs to grow, and
    // when it does, is expected to grow in larger steps.
    const DEFAULT_BUFFER_SIZE: usize = 1024 * 1024;

    // Protobufs can't exceed 2 GiB, if our observations grow this large, then
    // the profile as a whole would defintely exceed this.
    const MAX_CAPACITY: usize = i32::MAX as usize;

    pub fn try_new(sample_types_len: usize) -> io::Result<Self> {
        Ok(Self {
            compressed_timestamped_data: BufWriter::with_capacity(
                C::recommended_input_buf_size(),
                C::new_encoder(Self::DEFAULT_BUFFER_SIZE, Self::MAX_CAPACITY)?,
            ),
            sample_types_len,
        })
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

    pub fn try_into_iter(self) -> io::Result<TimestampedObservationsIterImpl<C>> {
        let encoder = self
            .compressed_timestamped_data
            .into_inner()
            .map_err(|e| e.into_error())?;
        Ok(TimestampedObservationsIterImpl {
            decoder: C::encoder_into_decoder(encoder)?,
            sample_types_len: self.sample_types_len,
        })
    }
}

impl<C: ObservationCodec> TimestampedObservationsIterImpl<C> {
    /// Decodes the next observation into `values`, reusing its existing allocation.
    pub fn next_into(&mut self, values: &mut Vec<i64>) -> Option<(Sample, Timestamp)> {
        // Read fixed-size header (stacktrace: u32, labels: u32, ts: i64) in one call.
        let mut header = [0u8; 16];
        self.decoder.read_exact(&mut header).ok()?;
        let stacktrace = u32::from_ne_bytes([header[0], header[1], header[2], header[3]]);
        let labels = u32::from_ne_bytes([header[4], header[5], header[6], header[7]]);
        let ts = i64::from_ne_bytes([
            header[8], header[9], header[10], header[11], header[12], header[13], header[14],
            header[15],
        ]);
        values.clear();
        for _ in 0..self.sample_types_len {
            values.push(self.decoder.read_i64::<NativeEndian>().ok()?);
        }
        Some((
            Sample {
                stacktrace: StackTraceId::from_offset(stacktrace as usize),
                labels: LabelSetId::from_offset(labels as usize),
            },
            std::num::NonZeroI64::new(ts)?,
        ))
    }
}
