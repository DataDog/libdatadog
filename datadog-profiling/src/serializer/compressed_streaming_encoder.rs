// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{mem, ops};
use datadog_alloc::buffer::FixedCapacityBuffer;
use datadog_alloc::vec::VirtualVec;
use datadog_profiling_core::protobuf::{self, Buffer, LenEncodable, MayGrowOps, NoGrowOps};
use lz4_flex::frame::FrameEncoder;
use std::io::{self, Write};

/// Serializes protobuf messages for pprof, and compresses them.
pub struct CompressedProtobufSerializer {
    storage: VirtualVec<u8>,
    zipper: FrameEncoder<Vec<u8>>,
}

#[inline]
#[cold]
fn cold() {}

#[inline]
fn likely(b: bool) -> bool {
    if !b {
        cold()
    }
    b
}

impl CompressedProtobufSerializer {
    /// Encodes the type in its in-wire protobuf format, and compresses it.
    ///
    /// # Errors
    /// If the zipper
    pub fn try_encode(&mut self, tag: u32, item: &impl LenEncodable) -> anyhow::Result<()> {
        let mut buffer = Buffer::try_from(&mut self.storage)?;
        let (len, needed) = protobuf::encoded_len(tag, item);
        if likely(needed <= buffer.remaining_capacity()) {
            unsafe { protobuf::encode_len_delimited(&mut buffer, tag, item, len) };
            return Ok(());
        }

        self.try_zip()?;
        let mut buffer = Buffer::try_from(&mut self.storage)?;
        buffer.try_reserve(needed)?;
        // SAFETY: checked there is adequate capacity.
        unsafe { protobuf::encode_len_delimited(&mut buffer, tag, item, len) };
        Ok(())
    }

    pub fn encode_varint(&mut self, tag: u32, value: u64) -> anyhow::Result<()> {
        let mut buffer = Buffer::try_from(&mut self.storage)?;
        let needed = protobuf::encode::tagged_varint_len(tag, value);
        if likely(needed <= buffer.remaining_capacity()) {
            unsafe { protobuf::encode::tagged_varint(&mut buffer, tag, value) };
            return Ok(());
        }

        self.try_zip()?;
        let mut buffer = Buffer::try_from(&mut self.storage)?;
        buffer.try_reserve(needed)?;
        unsafe { protobuf::encode::tagged_varint(&mut buffer, tag, value) };
        Ok(())
    }

    pub fn try_encode_str(&mut self, item: &str) -> io::Result<()> {
        // Handle strings differently. There's no point writing a big string
        // into the buffer, then to copy it into the zipper. Copy it straight
        // into the zipper instead.

        const STRING_KEY_LEN: usize =
            protobuf::encode::key_len(6, protobuf::encode::WireType::LengthDelimited);

        // SAFETY: MaybeUninit<[u8; N]> and [MaybeUninit<u8>; N] have the same
        // representation.
        let mut storage: [mem::MaybeUninit<u8>; STRING_KEY_LEN] =
            unsafe { mem::transmute(mem::MaybeUninit::<[u8; STRING_KEY_LEN]>::uninit()) };
        let mut fixed_cap = FixedCapacityBuffer::new(&mut storage);
        let mut buffer = Buffer::try_from(&mut fixed_cap)?;
        // SAFETY: the buffer has sufficient capacity from STRING_KEY_LEN.
        unsafe {
            protobuf::encode::key(&mut buffer, 6, protobuf::encode::WireType::LengthDelimited)
        }
        let encoded_tag = ops::Deref::deref(&buffer);

        // Encode the item.len() into a varint.
        // A 64-bit varint never takes more than 10 bytes to encode.
        const MAX_VARINT_LEN: usize = 10;

        // SAFETY: MaybeUninit<[u8; N]> and [MaybeUninit<u8>; N] have the same
        // representation.
        let mut storage: [mem::MaybeUninit<u8>; MAX_VARINT_LEN] =
            unsafe { mem::transmute(mem::MaybeUninit::<[u8; MAX_VARINT_LEN]>::uninit()) };
        let mut fixed_cap = FixedCapacityBuffer::new(&mut storage);
        let mut buffer = Buffer::try_from(&mut fixed_cap)?;
        let encoded_strlen = {
            unsafe { protobuf::encode::varint(&mut buffer, item.len() as u64) };
            ops::Deref::deref(&buffer)
        };

        self.zipper.write_all(encoded_tag)?;
        self.zipper.write_all(encoded_strlen)?;
        self.zipper.write_all(item.as_bytes())?;
        Ok(())
    }

    #[cold]
    #[inline(never)]
    pub fn try_zip(&mut self) -> io::Result<()> {
        self.zipper.write_all(&self.storage[..])?;
        self.storage.clear();
        Ok(())
    }

    pub fn finish(mut self) -> anyhow::Result<Vec<u8>> {
        self.try_zip()?;
        Ok(self.zipper.finish()?)
    }

    pub fn with_capacity(capacity: usize) -> anyhow::Result<Self> {
        let mut storage = VirtualVec::new();
        // The virtual allocator will round to a full page.
        storage.try_reserve(128)?;
        let mut frame_buffer = Vec::new();
        frame_buffer.try_reserve(capacity)?;
        let zipper = FrameEncoder::new(frame_buffer);
        Ok(Self { storage, zipper })
    }
}
