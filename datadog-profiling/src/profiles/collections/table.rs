use crate::profiles::collections::fam_ptr::FamPtr;
use crate::profiles::{ProfileError, ProfileId};
use crossbeam_utils::CachePadded;
use datadog_alloc::{Allocator, VirtualAllocator};
use hashbrown::HashTable;
use std::alloc::{Layout, LayoutError};
use std::hash::{Hash, Hasher};
use std::hint::unreachable_unchecked;
use std::mem::{needs_drop, MaybeUninit};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{RwLock, RwLockWriteGuard};
use std::{ptr, slice};

// If you change this layout at all, remember to change the layout method.
#[repr(C)]
struct TableImpl<T> {
    pub arc: CachePadded<AtomicUsize>,
    pub set: CachePadded<RwLock<HashTable<u32>>>,
    pub len: CachePadded<AtomicUsize>,
    pub data: [T],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct TableImplOffsets {
    arc: usize,
    set: usize,
    len: usize,
    data: usize,
}

/// Table isn't clone, but it does have [`Table::try_clone`].
#[repr(C)]
pub struct Table<T> {
    fam_ptr: FamPtr<T>,
}

impl<T> Table<T> {
    fn fatten(&self) -> &TableImpl<T> {
        unsafe { &*(self.fam_ptr.wide_object_ptr() as *mut TableImpl<T>) }
    }

    /// Returns the Layout needed to allocate
    fn layout(capacity: usize) -> Result<(Layout, TableImplOffsets), LayoutError> {
        let arc = Layout::new::<CachePadded<AtomicUsize>>();
        let set = Layout::new::<CachePadded<RwLock<HashTable<u32>>>>();
        let len = Layout::new::<CachePadded<AtomicUsize>>();
        let data = Layout::array::<T>(capacity)?;

        let (with_set, set_offset) = arc.extend(set)?;
        let (with_len, len_offset) = with_set.extend(len)?;
        let (with_data, data_offset) = with_len.extend(data)?;

        let offsets = TableImplOffsets {
            arc: 0,
            set: set_offset,
            len: len_offset,
            data: data_offset,
        };
        Ok((with_data.pad_to_align(), offsets))
    }

    pub fn try_with_capacity(cap: usize) -> Result<Table<T>, ProfileError> {
        if cap > i32::MAX as usize {
            return Err(ProfileError::InvalidInput);
        }

        let (layout, offsets) = Self::layout(cap)?;

        let mut ht = HashTable::new();

        // This is done before allocating the main object so that we don't
        // have to handle deallocation if this fails.
        // Shifting right by 2 is roughly the same as dividing by 4 but cheap.
        // The table will do its own rounding anyway.
        // SAFETY: empty hash table means the hasher will not be called.
        ht.try_reserve(cap >> 2, |_| unsafe { unreachable_unchecked() })?;

        let erased = VirtualAllocator
            .allocate(layout.pad_to_align())?
            .cast::<u8>();

        let fam_ptr = unsafe { FamPtr::new(erased, offsets.data, cap) };

        let fat = fam_ptr.wide_object_ptr() as *mut TableImpl<T>;
        unsafe {
            ptr::addr_of_mut!((*fat).arc).write(CachePadded::new(AtomicUsize::new(1)));
            ptr::addr_of_mut!((*fat).set).write(CachePadded::new(RwLock::new(ht)));
            ptr::addr_of_mut!((*fat).len).write(CachePadded::new(AtomicUsize::new(0)));
        }

        Ok(Table { fam_ptr })
    }

    /// Fetches the pointer to the underlying array, which will never
    /// change for a given [`Table`] object. The length is the "capacity" of
    /// the array, not the number that are initialized.
    fn array_ptr(&self) -> *mut [T] {
        self.fam_ptr.array_ptr()
    }

    /// Reserves a slice of length `n`, returning it as a slice of
    /// [`MaybeUninit`] data. The length of the vector is updated even if
    /// no data is ever written to the slice.
    fn try_reserve(
        &self,
        array_ptr: *mut [T],
        n: usize,
    ) -> Result<&mut [MaybeUninit<T>], ProfileError> {
        debug_assert!(n != 0);
        let inner = self.fatten();
        // Loading a relaxed value will perform better for single-threaded
        // library clients. For ones using threads, this value will be
        // updated in the loop below via a compare_exchange operation.
        let mut len = inner.len.load(Ordering::Relaxed);
        loop {
            let new_len = len.checked_add(n).ok_or(ProfileError::OutOfMemory)?;

            if new_len > array_ptr.len() {
                return Err(ProfileError::OutOfMemory);
            }
            match inner
                .len
                .compare_exchange(len, new_len, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(offset) => {
                    let ptr = unsafe { array_ptr.cast::<T>().add(offset).cast::<MaybeUninit<T>>() };
                    return Ok(unsafe { slice::from_raw_parts_mut(ptr, n) });
                }
                Err(old_len) => len = old_len,
            }
        }
    }

    /// Get the item associated with the [`ProfileId`]. If you want to get
    /// more than one, you probably want to use a [`TableVec`] to avoid
    /// multiple synchronizations of the table length.
    pub fn get(&self, id: ProfileId) -> Option<&T> {
        let offset = id.into_usize();
        self.fatten().data.get(offset)
    }
}

#[repr(C)]
pub struct TableVec<T> {
    table: Table<T>,
    cached_len: usize, // can be refreshed with "refresh"
}

impl<T> From<Table<T>> for TableVec<T> {
    fn from(table: Table<T>) -> Self {
        let inner = table.fatten();
        let cached_len = inner.len.load(Ordering::Acquire);
        Self { table, cached_len }
    }
}

/// A [`TableVec`] aliases the [`Table`]'s data, but it doesn't automatically
/// get updates to its len. Call [`TableVec::refresh`] to get an updated len.
/// A [`TableVec`] allows you to look up multiple ids without needed to
/// synchronize the length after every lookup.
impl<T> TableVec<T> {
    /// Try to make a [`TableVec`] from a [`Table`] by calling
    /// [``Table::try_clone`].
    pub fn try_new(table: &Table<T>) -> Result<TableVec<T>, ProfileError> {
        let cloned = table.try_clone()?;
        Ok(Self::from(cloned))
    }

    pub fn get(&self, id: ProfileId) -> Option<&T> {
        let offset = id.into_usize();
        (offset < self.cached_len)
            .then(|| unsafe { self.table.fatten().data.get_unchecked(offset) })
    }

    /// Refreshes the length of the vec from the table.
    pub fn refresh(&mut self) {
        self.cached_len = self.table.fatten().len.load(Ordering::Acquire);
    }

    pub fn iter(&self) -> impl Iterator<Item = (ProfileId, &T)> {
        let iter = self.table.fatten().data.iter();
        iter.enumerate()
            .map(|(offset, item)| (unsafe { ProfileId::new_unchecked(offset as u32) }, item))
    }
}

impl<T: Eq + Hash> Table<T> {
    /// Index into the pointer slice using the offset, returning a pointer.
    ///
    /// # Safety
    /// The offset should be in-bounds.
    #[cfg_attr(debug_assertions, track_caller)]
    unsafe fn add_debug_checked(ptr: *mut [T], offset: &u32) -> *mut T {
        let offset = *offset as usize;
        // This can help catch out-of-bounds operations, but won't catch issues
        // with getting offsets to uninitialized members.
        debug_assert!(offset < ptr.len());
        // SAFETY: it's supposed to be
        unsafe { ptr.cast::<T>().add(offset) }
    }

    // todo: document internal safety comments
    #[allow(clippy::unwrap_used)] // lock poisoning yuck
    pub fn insert(&self, value: T) -> Result<ProfileId, ProfileError> {
        // Hash the value outside of locking.
        let hash = Self::hash_one(&value);
        let array_ptr = self.array_ptr();

        let read_lock = self.fatten().set.read().unwrap();
        // SAFETY: all offsets in the set represent initialized members of the
        // array, so it's safe to index as well as dereference
        if let Some(offset_ref) = read_lock.find(hash, |off| unsafe {
            value.eq(&*Self::add_debug_checked(array_ptr, off))
        }) {
            let offset = *offset_ref;
            drop(read_lock);
            // SAFETY: capacity ensures it's <= i31::MAX.
            return Ok(unsafe { ProfileId::new_unchecked(offset) });
        }
        drop(read_lock);

        // Didn't exist, we need to add it. First insert it into the vec
        // and get its offset from the base, which is what we return. We
        // do this while the lock isn't held, because it's safe to do so
        // and reduces the amount of time the write lock is held.
        // Minimizing that time reduces contention with readers.
        let uninit = self.try_reserve(array_ptr, 1)?;
        let ptr = unsafe {
            let slot = uninit.get_unchecked_mut(0);
            slot.write(value);
            slot.as_mut_ptr()
        };
        // SAFETY: offset is from the correct base, base_ptr is stable
        // for the lifetime of the Table object.
        let offset = unsafe { ptr.offset_from(array_ptr.cast::<T>()) } as u32;
        // SAFETY: capacity ensures it's <= i31::MAX.
        let id = unsafe { ProfileId::new_unchecked(offset) };

        // Now put the stable pointer into the map.
        // SAFETY: we will modify the table in a way to keep it in a
        // consistent state.
        let mut write_lock = unsafe { self.acquire_write_lock() };
        write_lock.try_reserve(1, |offset| {
            Self::hash_one(unsafe { &*Self::add_debug_checked(array_ptr, offset) })
        })?;

        // SAFETY: the hasher is only called if items need to be moved, and
        // that is taken care of already in try_reserve.
        write_lock.insert_unique(hash, offset, |_| unsafe { unreachable_unchecked() });
        Ok(id)
    }

    /// Acquires the write lock for the underlying hash table.
    ///
    /// This is available as `pub` for code that handles forks, which
    /// needs to acquire all possible locks. This is tricky to do, but it
    /// is available for callers to try.
    ///
    /// # Safety
    ///
    /// Public users should not modify the hash table in any way, including
    /// but not limited to adding or removing items, changing items, and
    /// growing or shrinking the table.
    #[track_caller]
    pub unsafe fn acquire_write_lock(&self) -> RwLockWriteGuard<HashTable<u32>> {
        self.fatten().set.write().unwrap()
    }

    fn hash_one(value: &T) -> u64 {
        let mut hasher = rustc_hash::FxHasher::default();
        value.hash(&mut hasher);
        hasher.finish()
    }
}

impl<T> Table<T> {
    pub fn try_clone(&self) -> Result<Table<T>, ProfileError> {
        let arc = &self.fatten().arc;
        let rc = arc.fetch_add(1, Ordering::SeqCst);
        if rc >= i16::MAX as usize {
            arc.fetch_sub(1, Ordering::SeqCst);
            Err(ProfileError::RefcountOverflow)
        } else {
            let fam_ptr = self.fam_ptr;
            Ok(Table { fam_ptr })
        }
    }
}

impl<T> Drop for Table<T> {
    fn drop(&mut self) {
        let inner = self.fatten();
        let arc = inner.arc.fetch_sub(1, Ordering::SeqCst);
        if arc == 1 {
            let capacity = inner.data.len();
            #[allow(clippy::unwrap_used)] // lock poisoning yuck
            let mut lock = inner.set.write().unwrap();
            // drop the hash table inside the lock
            drop(core::mem::take(&mut *lock));
            if needs_drop::<T>() {
                let len = inner.len.load(Ordering::SeqCst);
                let base_ptr = self.array_ptr();
                let to_drop = unsafe { slice::from_raw_parts_mut(base_ptr.cast::<T>(), len) };
                unsafe {
                    ptr::drop_in_place(to_drop);
                }
            }
            drop(lock);

            // SAFETY: it exists, so it must be a valid layout.
            let (layout, _) = unsafe { Self::layout(capacity).unwrap_unchecked() };
            // SAFETY: refcount is 0, so it's safe to dealloc
            unsafe { VirtualAllocator.deallocate(self.fam_ptr.object_ptr(), layout) };
        }
    }
}
