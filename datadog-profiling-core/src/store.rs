// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::protobuf::{try_encode, Buffer, Identifiable};
use crate::TryReserveError;
use core::hash::Hasher;
use core::marker::PhantomData;
use datadog_alloc::buffer::MayGrowOps;
use hashbrown::HashTable;
use rustc_hash::FxHasher;

pub struct Store<T: Identifiable> {
    ht: HashTable<(ByteRange, u64)>,
    _marker: PhantomData<HashTable<(T, u64)>>,
}

impl<T: Identifiable> Default for Store<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Identifiable> Store<T> {
    pub fn new() -> Self {
        Self {
            ht: HashTable::new(),
            _marker: PhantomData,
        }
    }

    fn project(buffer: &impl MayGrowOps<u8>, byte_range: ByteRange) -> &[u8] {
        let range = core::ops::Range {
            start: byte_range.start.0 as usize,
            end: byte_range.end.0 as usize,
        };
        unsafe { buffer.get_unchecked(range) }
    }

    fn hash(bytes: &[u8]) -> u64 {
        let mut hasher = FxHasher::default();
        hasher.write(bytes);
        hasher.finish()
    }

    pub fn add<B: MayGrowOps<u8>>(
        &mut self,
        buffer: &mut Buffer<B>,
        tag: u32,
        item: T,
    ) -> Result<u64, TryReserveError> {
        let byte_range = try_encode(buffer, tag, &item)?;
        let bytes = Self::project(buffer, byte_range);
        let hash = Self::hash(bytes);

        let eq = |(byte_range, _id): &(ByteRange, u64)| {
            let bytes2 = Self::project(buffer, *byte_range);
            bytes == bytes2
        };
        if let Some((_item, existing_id)) = self.ht.find(hash, eq) {
            return Ok(*existing_id);
        }

        let hasher = |(byte_range, _id): &(ByteRange, u64)| {
            let bytes = Self::project(buffer, *byte_range);
            Self::hash(bytes)
        };
        if let Err(err) = self.ht.try_reserve(1, hasher) {
            return Err(match err {
                hashbrown::TryReserveError::CapacityOverflow => TryReserveError::CapacityOverflow,
                hashbrown::TryReserveError::AllocError { .. } => TryReserveError::AllocError,
            });
        }
        let id = item.id();
        _ = self.ht.insert_unique(hash, (byte_range, id), hasher);
        Ok(id)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.ht.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.ht.is_empty()
    }
}
