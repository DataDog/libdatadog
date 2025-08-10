// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A module for implementing Ids for slices of items, like strings which are
//! slices of bytes, and stack traces which are slices of location ids.
//!
//! Being able to look up IDs without acquiring a read-lock is trickier than
//! it is for regular Tables:
//!  1. You need a contiguous permanent region for storing `[*const [T]]` or a compressed 32-bit
//!     version. The offset of each item in that outer array is what we're handing out to users.
//!  2. You also need one or more permanent regions for storing the actual `T` values.
//! So we don't bother. Until proven otherwise, we'll just acquire a read-lock
//! to lookup values.

use super::CompressedPtrSlice;
use crate::profiles::collections::fam_ptr::FamPtr;
use crate::profiles::{ProfileError, ProfileId};
use crossbeam_utils::CachePadded;
use datadog_alloc::{Allocator, VirtualAllocator};
use hashbrown::HashTable;
use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::alloc::{Layout, LayoutError};
use std::hash::{Hash, Hasher};
use std::hint::unreachable_unchecked;
use std::mem::{needs_drop, MaybeUninit};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::{ptr, slice};

/// If you change this layout at all, remember to change the layout method.
/// The relationships between map and vec are important:
///  1. Always acquire the map lock before acquiring the vec lock.
///  2. Always keep their lengths in sync.
///
/// This using [`parking_lot::RwLock`] instead of the one in std to ensure that
/// the lock is fair, which means that the write lock cannot be starved by
/// readers. It also doesn't have lock poisoning.
#[repr(C)]
struct SliceTableImpl<T> {
    arc: CachePadded<AtomicUsize>,
    map: CachePadded<RwLock<HashTable<(CompressedPtrSlice, ProfileId)>>>,
    vec: CachePadded<RwLock<Vec<CompressedPtrSlice>>>,
    len: CachePadded<AtomicUsize>,
    data: [T],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct SliceTableImplOffsets {
    arc: usize,
    map: usize,
    vec: usize,
    len: usize,
    data: usize,
}

/// A table that works with slices of elements, such as stacks being a slice
/// of location ids.
#[repr(C)]
pub struct SliceTable<T> {
    fam_ptr: FamPtr<T>,
}

impl<T> SliceTable<T> {
    pub fn try_from_iter<I>(data_cap: usize, iter: I) -> Result<SliceTable<T>, ProfileError>
    where
        I: IntoIterator<Item = T> + ExactSizeIterator,
    {
        let collections_initial_size = iter.len();
        let (fam_ptr, layout) = Self::initialize(data_cap, collections_initial_size)?;
        let fat = fam_ptr.wide_object_ptr() as *mut SliceTableImpl<T>;

        let mut map = HashTable::new();
        // todo: HANDLE DEALLOC OF fam_ptr on failure of map and vec
        // SAFETY: empty hash table means the hasher will not be called.
        map.try_reserve(collections_initial_size, |_| unsafe {
            unreachable_unchecked()
        });
        let mut vec = Vec::new();
        vec.try_reserve(collections_initial_size)?;

        unsafe {
            ptr::addr_of_mut!((*fat).arc).write(CachePadded::new(AtomicUsize::new(1)));
            ptr::addr_of_mut!((*fat).map).write(CachePadded::new(RwLock::new(map)));
            ptr::addr_of_mut!((*fat).vec).write(CachePadded::new(RwLock::new(vec)));
            ptr::addr_of_mut!((*fat).len).write(CachePadded::new(AtomicUsize::new(0)));
        }

        Ok(Self { fam_ptr })
    }

    fn initialize(
        data_cap: usize,
        collections_initial_size: usize,
    ) -> Result<(FamPtr<T>, Layout), ProfileError> {
        // Ensure the number of bytes doesn't exceed i32 max, profiles
        // shouldn't be this large (pprof requirement)
        let in_bytes = data_cap
            .checked_mul(size_of::<T>())
            .ok_or(ProfileError::InvalidInput)?;
        if i32::try_from(in_bytes).is_err() {
            return Err(ProfileError::InvalidInput);
        }
        if i32::try_from(collections_initial_size).is_err() {
            return Err(ProfileError::InvalidInput);
        };

        // This doesn't make sense, caller probably made a mistake so we error
        // rather than just using the smaller of the two as the collections'
        // initial capacities.
        if collections_initial_size > data_cap {
            return Err(ProfileError::InvalidInput);
        }

        let (layout, offsets) = Self::layout(data_cap)?;

        let object_ptr = VirtualAllocator.allocate(layout)?.cast::<u8>();
        // SAFETY: all values are cohesive.
        Ok((
            unsafe { FamPtr::new(object_ptr, offsets.data, data_cap) },
            layout,
        ))
    }

    pub fn try_new(
        data_cap: usize,
        collections_initial_size: usize,
    ) -> Result<SliceTable<T>, ProfileError> {
        let (fam_ptr, layout) = Self::initialize(data_cap, collections_initial_size)?;
        let fat = fam_ptr.wide_object_ptr() as *mut SliceTableImpl<T>;

        let collections_initial_size = collections_initial_size.min(data_cap);

        let mut map = HashTable::new();
        // SAFETY: empty hash table means the hasher will not be called.
        if let Err(err) = map.try_reserve(collections_initial_size, |_| unsafe {
            unreachable_unchecked()
        }) {
            unsafe { VirtualAllocator.deallocate(fam_ptr.object_ptr(), layout) };
            return Err(err.into());
        }
        let mut vec = Vec::new();
        if let Err(err) = vec.try_reserve(collections_initial_size) {
            unsafe { VirtualAllocator.deallocate(fam_ptr.object_ptr(), layout) };
            return Err(err.into());
        }

        unsafe {
            ptr::addr_of_mut!((*fat).arc).write(CachePadded::new(AtomicUsize::new(1)));
            ptr::addr_of_mut!((*fat).map).write(CachePadded::new(RwLock::new(map)));
            ptr::addr_of_mut!((*fat).vec).write(CachePadded::new(RwLock::new(vec)));
            ptr::addr_of_mut!((*fat).len).write(CachePadded::new(AtomicUsize::new(0)));
        }

        Ok(Self { fam_ptr })
    }

    fn fatten(&self) -> &SliceTableImpl<T> {
        // SAFETY: the wide pointer represents an initialized SliceTableImpl.
        unsafe { &*(self.fam_ptr.wide_object_ptr() as *const SliceTableImpl<T>) }
    }

    /// Fetches a wide pointer to the underlying array, which will never
    /// change for a given [`SliceTable`] object. The elements are probably not
    /// fully initialized, so be careful to index only into the initialized
    /// portion.
    pub(crate) fn array_ptr(&self) -> *mut [T] {
        self.fam_ptr.array_ptr()
    }

    fn try_reserve(
        &self,
        array_ptr: *mut [T],
        n: usize,
    ) -> Result<*mut [MaybeUninit<T>], ProfileError> {
        let inner = self.fatten();
        // Loading a relaxed value will perform better for single-threaded
        // library clients. For ones using threads, this value will be
        // updated in the loop below via a compare_exchange operation.
        let mut len = inner.len.load(Ordering::Relaxed);
        let capacity = array_ptr.len();
        loop {
            let new_len = len.checked_add(n).ok_or(ProfileError::OutOfMemory)?;
            // This also ensures that it's not greater than i32::MAX, because
            // the array capacity in bytes is required to not exceed i32::MAX.
            if new_len > capacity {
                return Err(ProfileError::OutOfMemory);
            }
            match inner
                .len
                .compare_exchange(len, new_len, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(offset) => {
                    let ptr = unsafe { array_ptr.cast::<T>().add(offset) };
                    return Ok(unsafe { slice::from_raw_parts_mut(ptr.cast(), n) });
                }
                Err(old_len) => len = old_len,
            }
        }
    }

    fn layout(capacity: usize) -> Result<(Layout, SliceTableImplOffsets), LayoutError> {
        let arc = Layout::new::<CachePadded<AtomicU64>>();
        let map = Layout::new::<CachePadded<RwLock<HashTable<CompressedPtrSlice>>>>();
        let vec = Layout::new::<CachePadded<RwLock<Vec<CompressedPtrSlice>>>>();
        let len = Layout::new::<CachePadded<AtomicUsize>>();
        let data = Layout::array::<T>(capacity)?;

        let (with_map, map_offset) = arc.extend(map)?;
        let (with_vec, vec_offset) = with_map.extend(vec)?;
        let (with_len, len_offset) = with_vec.extend(len)?;
        let (with_data, data_offset) = with_len.extend(data)?;

        let offsets = SliceTableImplOffsets {
            arc: 0,
            map: map_offset,
            vec: vec_offset,
            len: len_offset,
            data: data_offset,
        };
        Ok((with_data.pad_to_align(), offsets))
    }

    /// Tries to get the slice backed by a single id. If you want to get
    /// multiple ids at once, you may want to use [`Self::as_slice`] to get
    /// a slice which holds the read-lock.
    pub fn get(&self, id: ProfileId) -> Option<&[T]> {
        let base_ptr = self.array_ptr();
        let vec = self.acquire_read_lock();
        let compressed_pointer_slice = vec.get(id.into_usize())?;
        Some(unsafe { &*compressed_pointer_slice.add_to(base_ptr) })
    }

    /// Gets a slice representation which holds the read-lock on the vector.
    /// This is more efficient than calling [`Self::get`] multiple times in a
    /// row.
    ///
    /// While this slice is alive, this partially blocks new items from being
    /// added to the table, so you should drop the slice as soon as possible.
    /// If you try to add new items to the table on the same thread that holds
    /// a slice, then
    pub fn as_slice(&self) -> SliceTableSlice<T> {
        let array_ptr = self.array_ptr();
        let vec = self.acquire_read_lock();
        SliceTableSlice { array_ptr, vec }
    }

    /// Acquires the write lock for the underlying collections.
    ///
    /// This is available as `pub` for code that handles forks, which
    /// needs to acquire all possible locks. This is tricky to do, but it
    /// is available for callers to try.
    ///
    /// This holds the write locks for the internal structures, so you cannot
    /// look up items or insert new ones, because you cannot re-obtain the
    /// read nor write locks!
    ///
    /// # Safety
    ///
    /// Public users should not modify the collections in any way, including
    /// but not limited to adding or removing items, changing items, and
    /// growing or shrinking.
    pub unsafe fn acquire_write_locks(
        &self,
    ) -> (
        RwLockWriteGuard<HashTable<(CompressedPtrSlice, ProfileId)>>,
        RwLockWriteGuard<Vec<CompressedPtrSlice>>,
    ) {
        let inner = self.fatten();
        let map = inner.map.write();
        let vec = inner.vec.write();
        (map, vec)
    }

    pub(crate) fn acquire_read_lock(&self) -> RwLockReadGuard<Vec<CompressedPtrSlice>> {
        self.fatten().vec.read()
    }

    pub fn try_clone(&self) -> Result<Self, ProfileError> {
        let arc = &self.fatten().arc;
        let rc = arc.fetch_add(1, Ordering::SeqCst);
        if rc >= i16::MAX as usize {
            arc.fetch_sub(1, Ordering::SeqCst);
            Err(ProfileError::OutOfMemory)
        } else {
            Ok(Self {
                fam_ptr: self.fam_ptr,
            })
        }
    }
}

impl<T: Copy + Eq + Hash> SliceTable<T> {
    fn hash_range(values: &[T]) -> u64 {
        let mut hasher = rustc_hash::FxHasher::default();
        values.hash(&mut hasher);
        hasher.finish()
    }

    pub fn insert(&self, values: &[T]) -> Result<ProfileId, ProfileError> {
        // Hash the value outside of locking.
        let hash = Self::hash_range(&values);
        let array_ptr = self.array_ptr();
        let len = values.len();
        let inner = self.fatten();

        // scoped to limit lock lifetime
        {
            let map = inner.map.read();

            // SAFETY: the base_ptr is stable for the entire lifetime of the object
            // so the CompressedPtrSlice remains in bounds.
            if let Some((_, id)) = map.find(hash, |(off, _)| unsafe {
                &*off.add_to(array_ptr) == values
            }) {
                return Ok(*id);
            }
        }

        // Didn't exist, we need to add it. First insert the range into the
        // underlying storage, then add
        // We do this while the lock isn't held, because it's safe to do so
        // and reduces the amount of time the write lock is held.
        // Minimizing that time reduces contention with readers.
        let compressed_ptr_slice = {
            let uninit = self.try_reserve(array_ptr, len)?;
            let src =
                unsafe { slice::from_raw_parts(values.as_ptr().cast::<MaybeUninit<T>>(), len) };
            unsafe { (&mut *uninit).copy_from_slice(src) };
            let offset = unsafe { uninit.cast::<T>().offset_from(array_ptr.cast::<T>()) as usize };
            CompressedPtrSlice::new(offset as u32, len as u32)
        };

        // Now put the stable pointer into the collections.
        let inner = self.fatten();

        let mut map = inner.map.write();
        map.try_reserve(1, |(cps, _)| {
            Self::hash_range(unsafe { &*cps.add_to(array_ptr) })
        })?;
        let offset = map.len();

        // Need to avoid a situation where the vec or map has room for an item,
        // but the other one does not. So before the map actually adds the
        // item, get the vec lock and reserve the memory.
        let mut vec = inner.vec.write();
        vec.try_reserve(1)?;
        vec.push(compressed_ptr_slice);
        drop(vec);

        let id = unsafe { ProfileId::new_unchecked(offset as u32) };
        // SAFETY: the hasher is only called if items need to be moved, and
        // that is taken care of already in try_reserve.
        map.insert_unique(hash, (compressed_ptr_slice, id), |_| unsafe {
            unreachable_unchecked()
        });
        drop(map);
        // SAFETY: the underlying data is restricted to i32::MAX, so there's
        // no way the map's length can be larger than i32::MAX.
        Ok(id)
    }
}

pub struct SliceTableSlice<'a, T> {
    array_ptr: *mut [T],
    vec: RwLockReadGuard<'a, Vec<CompressedPtrSlice>>,
}

impl<'a, T> SliceTableSlice<'a, T> {
    pub fn get(&self, id: ProfileId) -> Option<&[T]> {
        let compressed_pointer_slice = self.vec.get(id.into_usize())?;
        Some(unsafe { &*compressed_pointer_slice.add_to(self.array_ptr) })
    }
}

impl<T> Drop for SliceTable<T> {
    fn drop(&mut self) {
        let inner = self.fatten();
        let arc = inner.arc.fetch_sub(1, Ordering::SeqCst);
        if arc == 1 {
            let capacity = inner.data.len();
            if needs_drop::<T>() {
                let len = inner.len.load(Ordering::SeqCst);
                let base_ptr = self.array_ptr().cast::<T>();
                for i in 0..len {
                    unsafe { ptr::drop_in_place(base_ptr.add(i)) };
                }
            }

            // SAFETY: it exists, so it must be a valid layout.
            let (layout, _offsets) = unsafe { Self::layout(capacity).unwrap_unchecked() };
            // SAFETY: refcount is 0, so it's safe to dealloc
            unsafe { VirtualAllocator.deallocate(self.fam_ptr.object_ptr(), layout) };
        }
    }
}
