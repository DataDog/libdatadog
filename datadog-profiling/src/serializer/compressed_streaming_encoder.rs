// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::mem;
use datadog_alloc::buffer::FixedCapacityBuffer;
use datadog_alloc::vec::VirtualVec;
use datadog_profiling_core::protobuf::encode::{tagged_varint, tagged_varint_len, MAX_TAG};
use datadog_profiling_core::protobuf::{encode_len_delimited, LenEncodable};
use lz4_flex::frame::FrameEncoder;
use std::io::{self, Write};

/// Serializes protobuf messages for pprof, and compresses them.
pub struct CompressedProtobufSerializer {
    encoder: FrameEncoder<VirtualVec<u8>>,
}

impl CompressedProtobufSerializer {
    /// Encodes the type in its in-wire protobuf format, and compresses it.
    pub fn try_encode(&mut self, tag: u32, item: &impl LenEncodable) -> io::Result<()> {
        encode_len_delimited(&mut self.encoder, tag, item)
    }

    #[inline]
    pub fn encode_varint(&mut self, tag: u32, value: u64) -> io::Result<()> {
        const BUFFER_LEN: usize = tagged_varint_len(MAX_TAG, u64::MAX);
        let mut storage: [mem::MaybeUninit<u8>; BUFFER_LEN] =
            unsafe { mem::transmute(mem::MaybeUninit::<[u8; BUFFER_LEN]>::uninit()) };
        let mut buf = FixedCapacityBuffer::from(storage.as_mut_slice());
        unsafe { tagged_varint(&mut buf, tag, value) };
        self.encoder.write_all(buf.as_slice())
    }

    pub fn finish(self) -> anyhow::Result<VirtualVec<u8>> {
        Ok(self.encoder.finish()?)
    }

    pub fn with_capacity(capacity: usize) -> io::Result<Self> {
        let mut storage = VirtualVec::new();
        // The virtual allocator will round to a full page.
        storage.try_reserve(capacity)?;
        let encoder = FrameEncoder::new(storage);
        Ok(Self { encoder })
    }
}
