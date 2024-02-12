use super::{InternError, LengthPrefixedStr, StringAllocator};
use crate::alloc::r#virtual::raw::{virtual_alloc, virtual_free};
use crate::alloc::{pad_to, page_size, AllocError};
use std::alloc::Layout;
use std::num::NonZeroU32;
use std::ops::Deref;
use std::ptr::{addr_of_mut, NonNull};
use std::sync::atomic::{AtomicU32, Ordering};
use std::{io, mem, slice};

/// PRIVATE TYPE.
/// The allocation header for an arena, which tracks information about the
/// allocation as well as holding the data directly.
#[repr(C)]
struct ArenaHeader<T: Copy> {
    allocation_size: u32,
    rc: AtomicU32,
    len: AtomicU32,
    // This at least `allocation_size` / mem::sizeof::<T>() in size.
    data: [mem::MaybeUninit<T>; 0],
}

#[repr(C)]
pub struct ArenaSlice<T: Copy> {
    // "borrows" the pointer. It cannot add new items.
    ptr: Option<NonNull<ArenaHeader<T>>>,
}

impl<T: Copy> Clone for ArenaSlice<T> {
    fn clone(&self) -> Self {
        match self.ptr {
            None => Self { ptr: None },
            Some(nonnull) => {
                // SAFETY: since the ArenaVec holds a reference to the data, it
                // will still be alive.
                let header = unsafe { nonnull.as_ref() };

                // todo: should we consider overflows here and abort?
                header.rc.fetch_add(1, Ordering::SeqCst);

                Self { ptr: Some(nonnull) }
            }
        }
    }
}

impl<T: Copy + Sized> ArenaHeader<T> {
    #[track_caller]
    fn layout(capacity: usize) -> (Layout, usize) {
        _ = u32::try_from(capacity).expect("arena capacity to fit in u32");
        let array = Layout::array::<T>(capacity).expect("arena array layout to succeed");
        Layout::new::<Self>()
            .extend(array)
            .expect("arena header layout to succeed")
    }
}

#[repr(C)]
pub struct ArenaVec<T: Copy> {
    /// "owns" the header, meaning it is allowed to append new items, but it
    /// cannot mutate existing items. When appending, be careful to write the
    /// item before atomically increasing the length.
    header_ptr: Option<NonNull<ArenaHeader<T>>>,

    /// Points to the beginning of the space for items to be stored in the
    /// mapping, properly aligned for a `T`.
    data_ptr: NonNull<T>,

    /// The number of items in the arena. This is duplicate information but
    /// makes it nicer for certain functions to not need to interact with
    /// atomics.
    length: u32,

    /// The total number of `T`s that can be stored in the arena. Unchanged
    /// after creation.
    capacity: u32,
}

impl<T: Copy> Drop for ArenaVec<T> {
    fn drop(&mut self) {
        if let Some(nonnull) = self.header_ptr.take() {
            // SAFETY: since the ArenaVec holds a reference to the data, it
            // will still be alive.
            let header = unsafe { nonnull.as_ref() };
            if header.rc.fetch_sub(1, Ordering::SeqCst) == 1 {
                // Safety: passing pointer and size un-changed.
                let _result =
                    unsafe { virtual_free(nonnull.cast(), header.allocation_size as usize) };

                #[cfg(debug_assertions)]
                if let Err(err) = _result {
                    panic!("failed to drop ArenaVec: {err:#}");
                }
            }
        }
    }
}

/// # Safety
/// The struct only holds a pointer to the real data. Since it owns the header,
/// it can be moved to another thread without issue.
unsafe impl<T: Copy> Send for ArenaVec<T> {}

impl<T: Copy> ArenaVec<T> {
    pub fn with_capacity_in_bytes(min_bytes: usize) -> io::Result<Self> {
        if min_bytes == 0 {
            return Ok(Self {
                header_ptr: None,
                rc: NonNull::dangling(),
                len: NonNull::dangling(),
                data: NonNull::dangling(),
                capacity: 0,
            });
        }

        let page_size = page_size();
        // Need to ensure there is room for the header.
        let min_bytes = min_bytes.max(mem::size_of::<ArenaHeader<T>>());
        match pad_to(min_bytes, page_size) {
            None => return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("requested virtual allocation of {min_bytes} bytes could not be padded to the page size {page_size}"),
            )),
            Some(allocation_size) => unsafe {
                let unadjusted_capacity = match u32::try_from(allocation_size) {
                    Ok(cap) => cap,
                    Err(_err) => return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("padded virtual allocation of {allocation_size} bytes did not fit in u32"),
                    )),
                };

                let nonnull = virtual_alloc(allocation_size)?.cast::<ArenaHeader<T>>();
                let header = nonnull.as_ptr();
                addr_of_mut!((*header).allocation_size).write(unadjusted_capacity);
                addr_of_mut!((*header).rc).write(AtomicU32::new(1));
                addr_of_mut!((*header).len).write(AtomicU32::new(0));
                // will not underflow, min_bytes was .max'd with size of the header.
                let capacity = unadjusted_capacity - mem::size_of::<ArenaHeader<T>>() as u32;

                Ok(Self {
                    header_ptr: Some(nonnull),
                    rc: NonNull::new_unchecked(addr_of_mut!((*header).rc)),
                    len: NonNull::new_unchecked(addr_of_mut!((*header).len)),
                    data: NonNull::new_unchecked(addr_of_mut!((*header).data).cast()),
                    capacity,
                })
            }
        }
    }

    fn header(&self) -> Option<&ArenaHeader<T>> {
        match self.header_ptr.as_ref() {
            None => None,
            // SAFETY: ArenaVec holds a reference to the data.
            Some(ptr) => Some(unsafe { ptr.as_ref() }),
        }
    }

    fn try_reserve(&self, additional: u32) -> Result<NonNull<[T]>, AllocError> {
        if self.header_ptr.is_none() {
            return Err(AllocError);
        }
        let len = unsafe { self.len.as_ref().load(Ordering::Acquire) };
        if self.capacity - len < additional {
            Err(AllocError)
        } else {
            // SAFETY: todo
            let addr = unsafe { self.data.as_ptr().add(len as usize) };
            // SAFETY: todo
            Ok(unsafe {
                NonNull::new_unchecked(core::ptr::slice_from_raw_parts_mut(
                    addr,
                    additional as usize,
                ))
            })
        }
    }

    pub fn base_ptr(&self) -> Option<NonNull<T>> {
        if self.header_ptr.is_none() {
            None
        } else {
            Some(self.data)
        }
    }
}

impl<T: Copy> ArenaSlice<T> {
    fn header(&self) -> Option<&ArenaHeader<T>> {
        match self.ptr.as_ref() {
            None => None,
            // SAFETY: slice has a reference count, will be alive.
            Some(nonull) => unsafe { Some(nonull.as_ref()) },
        }
    }
}

impl<T: Copy> Deref for ArenaHeader<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        let ptr = self.data.as_ptr().cast();
        let len = self.len.load(Ordering::Acquire) as usize;
        // SAFETY: ArenaHeader::layout() aligned it correctly, and  the first
        // `len` are properly initialized.
        unsafe { slice::from_raw_parts(ptr, len) }
    }
}

impl<T: Copy> Deref for ArenaSlice<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        match self.header() {
            None => &[],
            Some(header) => header.deref(),
        }
    }
}

impl<T: Copy> Deref for ArenaVec<T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        match self.header() {
            None => &[],
            Some(header) => header.deref(),
        }
    }
}

struct IndexableStringArenaAllocator {
    bytes_storage: ArenaVec<u8>,
}

impl IndexableStringArenaAllocator {
    fn with_capacity(min_bytes: usize) -> io::Result<Self> {
        let bytes_storage = ArenaVec::with_capacity_in_bytes(min_bytes)?;
        Ok(Self { bytes_storage })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct IndexableHandle(NonZeroU32);

impl StringAllocator for IndexableStringArenaAllocator {
    type Handle = IndexableHandle;

    fn allocate_str(&self, value: impl AsRef<str>) -> Result<IndexableHandle, InternError> {
        let str = value.as_ref();
        let n = match u16::try_from(str.len()) {
            Ok(u) => u,
            Err(_) => return Err(InternError::LargeString(str.len())),
        };
        let layout = LengthPrefixedStr::layout_of(n)?;
        let layout_size = layout.size() as u32;
        // Ensure there's room before inserting.
        let storage_ptr = self.bytes_storage.try_reserve(layout_size)?;

        // Both arenas have room, so go ahead and actually allocate.
        let length_prefixed_str = unsafe { LengthPrefixedStr::from_str_in(str, n, storage_ptr) };
        unsafe {
            _ = self
                .bytes_storage
                .len
                .as_ref()
                .fetch_add(layout_size, Ordering::SeqCst)
        };

        // SAFETY: the LengthPrefixedStr necessarily points to something at
        // least 2 bytes because of the length prefix.
        let handle_offset =
            unsafe { length_prefixed_str.as_ptr().add(mem::size_of::<u16>()) as u32 };
        // SAFETY: the offset is always at least +2 because of length prefix.
        Ok(IndexableHandle(unsafe {
            NonZeroU32::new_unchecked(handle_offset)
        }))
    }

    fn fetch(&self, handle: Option<Self::Handle>) -> &str {
        match handle {
            None => "",

            // SAFETY: the  lifetime of the str _is_ the allocator's lifetime.
            Some(h) => unsafe {
                mem::transmute(self.convert_handle_to_length_prefixed_str(h).deref())
            },
        }
    }

    fn capacity(&self) -> usize {
        self.bytes_storage.capacity as usize
    }

    fn convert_handle_to_length_prefixed_str(&self, handle: Self::Handle) -> LengthPrefixedStr {
        // SAFETY: All handles are guaranteed to fit within the allocation, or
        // the operation would have failed, so there must be an allocation.
        let ptr = unsafe { self.bytes_storage.base_ptr().unwrap_unchecked() };

        // SAFETY: Again, all handles are guaranteed to fit within the
        // allocation, or else the operation would have initially failed.
        let handle_adusted_ptr = unsafe { ptr.as_ptr().add(handle.0.get() as usize) };

        // SAFETY: the handle offsets point to the data, meaning they are just
        // past the length-prefix header, so we can always take off the size
        // of the header from this ptr.
        let offset_ptr = unsafe { handle_adusted_ptr.sub(mem::size_of::<u16>()) };

        // SAFETY: the pointer now points at the length-prefix string header,
        // which is the repr for LengthPrefixedStr.
        unsafe { mem::transmute::<*mut u8, LengthPrefixedStr>(offset_ptr) }
    }

    fn convert_length_prefixed_str_to_handle(&self, str: LengthPrefixedStr) -> Self::Handle {
        let data_ptr = str.data_ptr().as_ptr() as *const u8;

        // SAFETY: length-prefixed strings must point inside the allocator's
        // or else something is already wrong. So there must be a mapping
        // backing the arena if we have a length-prefixed string.
        let base_ptr = unsafe { self.bytes_storage.base_ptr().unwrap_unchecked().as_ptr() };

        // SAFETY:
        //  1. The pointers belong to the same virtual allocation, and are in
        //     bounds (or one byte past the allocation, if it's full).
        //  2. Pointers are both working with u8s.
        //  3. On 64-bit, this won't exceed isize as we ensure the mappings
        //     are a maximum of u32::MAX.
        let offset = unsafe { data_ptr.offset_from(base_ptr.cast()) };

        debug_assert!(offset.is_positive());

        let nonnegative = offset as usize;
        let result = u32::try_from(nonnegative);
        debug_assert!(result.is_ok());
        // SAFETY: since both pointers were in-bounds of the allocation, and
        // the header takes up at least one byte, the difference cannot exceed
        // u32 given a maximal mapping size of u32.
        let small_offset = unsafe { result.unwrap_unchecked() };

        // SAEFTY: non-zero because the data ptr will be prefixed by a header.
        IndexableHandle(unsafe { NonZeroU32::new_unchecked(small_offset) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collections::StringTable;

    #[test]
    fn test_header_layout() {
        let (layout, data_offset) = ArenaHeader::<*const ()>::layout(1);
        assert_eq!(data_offset, mem::size_of::<ArenaHeader<*const ()>>());
        assert_eq!(
            layout.size(),
            mem::size_of::<ArenaHeader<*const ()>>() + mem::size_of::<*const ()>()
        );
    }

    #[test]
    fn test_string_table() {
        let allocator = IndexableStringArenaAllocator::with_capacity(4096).unwrap();
        let mut string_table = StringTable::new_in(allocator).unwrap();
        string_table.intern("datadog").unwrap();
    }
}
