// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use crate::alloc::{AllocError, ArenaAllocator};
use crate::collections::identifiable::{Id, StringId};
use core::borrow::Borrow;
use core::ops::Deref;
use core::{fmt, hash, mem, ptr};
use std::alloc::{Layout, LayoutError};
use std::collections::TryReserveError;

pub trait StringAllocator {
    type Handle: Copy + Sized;

    /// Copies the given str into the allocator.
    fn allocate_str(&self, str: impl AsRef<str>) -> Result<Self::Handle, InternError>;

    fn fetch(&self, handle: Option<Self::Handle>) -> &str;

    fn capacity(&self) -> usize;

    fn convert_handle_to_length_prefixed_str(&self, handle: Self::Handle) -> LengthPrefixedStr;
    fn convert_length_prefixed_str_to_handle(&self, str: LengthPrefixedStr) -> Self::Handle;
}

impl StringAllocator for ArenaAllocator {
    type Handle = LengthPrefixedStr;

    fn allocate_str(&self, value: impl AsRef<str>) -> Result<LengthPrefixedStr, InternError> {
        let str = value.as_ref();
        match u16::try_from(str.len()) {
            Ok(n) => {
                let layout = match LengthPrefixedStr::layout_of(n) {
                    Ok(l) => l,
                    Err(_err) => return Err(InternError::LargeString(str.len())),
                };
                let allocation = self.allocate_zeroed(layout)?;
                Ok(unsafe { LengthPrefixedStr::from_str_in(str, n, allocation) })
            }
            Err(_) => Err(InternError::AllocError),
        }
    }

    fn fetch(&self, handle: Option<Self::Handle>) -> &str {
        match handle {
            None => "",

            // SAFETY: the real lifetime of the LengthPrefixedStr _is_ the
            // allocator's lifetime.
            Some(h) => unsafe { mem::transmute(h.deref()) },
        }
    }

    fn capacity(&self) -> usize {
        match &self.mapping {
            None => 0,
            Some(mapping) => mapping.allocation_size(),
        }
    }

    fn convert_handle_to_length_prefixed_str(&self, handle: Self::Handle) -> LengthPrefixedStr {
        handle
    }

    fn convert_length_prefixed_str_to_handle(&self, str: LengthPrefixedStr) -> Self::Handle {
        str
    }
}

type FxHashMap<K, V> =
    std::collections::HashMap<K, V, hash::BuildHasherDefault<rustc_hash::FxHasher>>;

/// Not pub, used to do unsafe things. See [LengthPrefixedStr] for more info.
#[repr(C)]
struct LengthPrefixedHeader {
    /// Stored as a byte array to avoid alignments greater than 1. This
    /// prevents wasted bytes in the arena due to alignment.
    len: [u8; mem::size_of::<u16>()],
}

/// Dangerous type, has a lifetime which has been elided! The lifetime is the
/// lifetime of the arena. It points to a struct which looks like this:
///
/// ```
/// #[repr(C)]
/// struct LengthPrefixedString {
///     /// The length of the string, in native byte order as a [u16].
///     size: [u8; core::mem::size_of::<u16>()],
///     /// The string data, which is `size` bytes and is not guaranteed to
///     /// end with the null byte.
///     data: [u8],
/// }
/// ```
///
/// In stable Rust at this time, there's no way to use thin-pointers to these
/// types and then be able to re-constitute the fat-pointer at run-time. This
/// is why [LengthPrefixedStr] uses a thin-pointer to only the header prefix
/// of the data and uses unsafe Rust for the rest.
///
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LengthPrefixedStr {
    header: ptr::NonNull<LengthPrefixedHeader>,
}

impl fmt::Debug for LengthPrefixedStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s: &str = self;
        s.fmt(f)
    }
}

impl PartialEq for LengthPrefixedStr {
    fn eq(&self, other: &Self) -> bool {
        // delegate to Deref<Target=str>
        **self == **other
    }
}

impl Eq for LengthPrefixedStr {}

impl hash::Hash for LengthPrefixedStr {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        let str: &str = self;
        str.hash(state)
    }
}

impl Borrow<str> for LengthPrefixedStr {
    fn borrow(&self) -> &str {
        self
    }
}

impl LengthPrefixedStr {
    pub fn layout_of(n: u16) -> Result<Layout, LayoutError> {
        let header = Layout::new::<LengthPrefixedHeader>();
        let array = Layout::array::<u8>(n as usize)?;
        let (layout, offset) = header.extend(array)?;
        debug_assert_eq!(offset, mem::size_of::<u16>());
        // no need to pad, everything is alignment of 1
        Ok(layout)
    }

    pub fn data_ptr(self) -> ptr::NonNull<[u8]> {
        let header = self.header.as_ptr();
        // SAFETY: a LengthPrefixedStr always points to a valid header object,
        // even for an empty string.
        let len = u16::from_be_bytes(unsafe { &*header }.len) as usize;

        // SAFETY: the data begins immediately after the header.
        let ptr = unsafe { self.header.as_ptr().add(1) }.cast();
        let fatptr = ptr::slice_from_raw_parts_mut(ptr, len);

        // SAFETY: the data points inside an allocated object (not null).
        unsafe { ptr::NonNull::new_unchecked(fatptr) }
    }

    /// The ptr needs to point to a valid length prefixed string.
    unsafe fn from_bytes(ptr: ptr::NonNull<u8>) -> Self {
        Self { header: ptr.cast() }
    }

    #[inline]
    pub unsafe fn from_str_in(s: &str, n: u16, ptr: ptr::NonNull<[u8]>) -> Self {
        // SAFETY: todo
        let header_src = u16::to_be_bytes(n);
        debug_assert!(header_src.len() + n as usize <= ptr.len());

        let header_ptr = ptr.as_ptr() as *mut u8;
        // SAFETY: todo
        ptr::copy_nonoverlapping(header_src.as_ptr(), header_ptr, header_src.len());
        // SAFETY: todo
        let bytes_ptr = header_ptr.add(header_src.len());
        // SAFETY: todo
        ptr::copy_nonoverlapping(s.as_ptr(), bytes_ptr, n as usize);
        Self {
            // SAFETY: todo
            header: ptr::NonNull::new_unchecked(header_ptr.cast()),
        }
    }
}

impl Deref for LengthPrefixedStr {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        let fatptr = self.data_ptr();
        // SAFETY: todo
        unsafe { core::str::from_utf8_unchecked(fatptr.as_ref()) }
    }
}

/// A StringTable holds unique strings in a set. The data of each string is
/// held in an arena, so individual strings don't hit the system allocator.
pub struct StringTable<A: StringAllocator = ArenaAllocator> {
    arena: A,
    map: FxHashMap<LengthPrefixedStr, StringId>,
}

#[derive(Debug)]
pub enum InternError {
    /// The string is too big. Current limit is [u16::MAX]. Holds the size of
    /// the string which tried to get allocated.
    LargeString(usize),
    /// One of the underlying allocators failed to allocate memory.
    AllocError,
}

impl fmt::Display for InternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InternError::LargeString(size) => write!(f, "string is too large to intern: {size}"),
            InternError::AllocError => write!(f, "string table is out-of-memory"),
        }
    }
}

impl std::error::Error for InternError {}

impl From<AllocError> for InternError {
    fn from(_value: AllocError) -> Self {
        InternError::AllocError
    }
}

impl From<LayoutError> for InternError {
    fn from(_value: LayoutError) -> Self {
        InternError::AllocError
    }
}

impl From<TryReserveError> for InternError {
    fn from(_value: TryReserveError) -> Self {
        InternError::AllocError
    }
}

impl StringTable<ArenaAllocator> {
    /// Creates a new string table whose arena allocator can hold at least
    /// `min_capacity` bytes. This will get rounded up to multiple of the OS
    /// page size. A capacity of 0 is allowed, in which case a virtual
    /// allocation will not be performed.
    pub fn with_arena_capacity(min_capacity: usize) -> anyhow::Result<Self> {
        let arena = ArenaAllocator::with_capacity(min_capacity.next_power_of_two())?;
        Ok(Self::new_in(arena)?)
    }

    pub fn iter(&self) -> StringTableIter {
        StringTableIter {
            arena: &self.arena,
            offset: 0,
            len: self.map.len(),
            has_empty_str: true,
        }
    }
}

impl<A: StringAllocator> StringTable<A> {
    pub fn new_in(arena: A) -> Result<Self, AllocError> {
        let map = FxHashMap::default();
        Ok(StringTable { arena, map })
    }

    pub fn arena(&self) -> &A {
        &self.arena
    }

    /// Like [StringTable::intern], but on success returns a tuple of the
    /// [LengthPrefixedStr] and [StringId] rather than just a [StringId]. The
    /// caller needs to be sure to only use the [LengthPrefixedStr] while the
    /// arena is still valid.
    #[inline(never)]
    pub(crate) fn insert_full<S>(
        &mut self,
        s: &S,
    ) -> Result<(Option<A::Handle>, StringId), InternError>
    where
        S: ?Sized + Borrow<str>,
    {
        self.intern_inner(s)
    }

    pub(crate) fn intern_inner<S>(
        &mut self,
        s: &S,
    ) -> Result<(Option<A::Handle>, StringId), InternError>
    where
        S: ?Sized + Borrow<str>,
    {
        let str = s.borrow();
        if str.is_empty() {
            return Ok((None, StringId::ZERO));
        }

        match self.map.get_key_value(str) {
            None => {
                self.map.try_reserve(1)?;
                let string_id = StringId::from_offset(self.map.len());
                let arena = &self.arena;
                let handle = arena.allocate_str(str)?;
                self.map.insert(
                    arena.convert_handle_to_length_prefixed_str(handle),
                    string_id,
                );
                Ok((Some(handle), string_id))
            }
            Some((str, string_id)) => {
                let handle = self.arena.convert_length_prefixed_str_to_handle(*str);
                Ok((Some(handle), *string_id))
            }
        }
    }

    /// Inserts the string into the string table, and returns a [StringId]
    /// which represents the order in which the string was first inserted into
    /// the table.
    ///
    /// Returns an error if the string is longer than [u16::MAX] or if one of
    /// the underlying allocator fails to allocate memory.
    #[inline(never)]
    pub fn intern<S>(&mut self, s: &S) -> Result<StringId, InternError>
    where
        S: ?Sized + Borrow<str>,
    {
        match self.intern_inner(s) {
            Ok((_str, string_id)) => Ok(string_id),
            Err(err) => Err(err),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        // + 1 for empty string, which isn't held
        self.map.len() + 1
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        // always holds the empty string, is never empty
        false
    }

    #[inline]
    pub fn arena_capacity(&self) -> usize {
        self.arena.capacity()
    }
}

pub struct StringTableIter<'a> {
    arena: &'a ArenaAllocator,
    /// Offset from the arena's base pointer for the next item, which is a
    /// length-prefixed string, see [LengthPrefixedStr] for layout info.
    offset: usize,
    /// The number of items remaining in the iterator.
    len: usize,
    /// True if the empty string hasn't been yielded yet.
    has_empty_str: bool,
}

impl<'a> Iterator for StringTableIter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.len == 0 {
            None
        } else if self.has_empty_str {
            self.len -= 1;
            self.has_empty_str = false;
            Some("")
        } else {
            let ptr = self
                .arena
                .mapping
                .as_ref()
                .unwrap()
                .base_non_null_ptr::<u8>()
                .as_ptr();

            // SAFETY: todo
            let str = unsafe {
                let ptr = ptr.add(self.offset);
                let ptr = ptr::NonNull::new_unchecked(ptr);
                mem::transmute::<&str, &'a str>(LengthPrefixedStr::from_bytes(ptr).deref())
            };
            self.len -= 1;
            self.offset += mem::size_of::<u16>() + str.len();
            Some(str)
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.len, Some(self.len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::hash::{BuildHasher, BuildHasherDefault, Hash, Hasher};
    use std::collections::HashMap;

    #[test]
    fn test_prefix_strings() -> anyhow::Result<()> {
        let layout = LengthPrefixedStr::layout_of(3)?;
        assert_eq!(layout.size(), 5);

        let arena = ArenaAllocator::with_capacity(32)?;

        let str = arena.allocate_str("datadog")?;

        assert_eq!(&*str, "datadog");

        let build_hasher = BuildHasherDefault::<rustc_hash::FxHasher>::default();
        let mut hasher = build_hasher.build_hasher();
        str.hash(&mut hasher);
        let a = hasher.finish();

        let mut hasher = build_hasher.build_hasher();
        "datadog".hash(&mut hasher);
        let b = hasher.finish();

        assert_eq!(a, b);

        let mut map: HashMap<LengthPrefixedStr, u32> = HashMap::new();
        map.insert(str, 1);

        map.get("datadog").unwrap();
        Ok(())
    }

    #[test]
    fn owned_string_table() -> anyhow::Result<()> {
        let cases: &[_] = &[
            (StringId::ZERO, ""),
            (StringId::from_offset(1), "local root span id"),
            (StringId::from_offset(2), "span id"),
            (StringId::from_offset(3), "trace endpoint"),
            (StringId::from_offset(4), "samples"),
            (StringId::from_offset(5), "count"),
            (StringId::from_offset(6), "wall-time"),
            (StringId::from_offset(7), "nanoseconds"),
            (StringId::from_offset(8), "cpu-time"),
            (StringId::from_offset(9), "<?php"),
            (StringId::from_offset(10), "/srv/demo/public/index.php"),
            (StringId::from_offset(11), "pid"),
        ];

        let capacity = cases.iter().map(|(_, str)| str.len()).sum();

        let mut table = StringTable::with_arena_capacity(capacity)?;

        // The empty string must always be included in the table at 0.
        let empty_table = table.iter().collect::<Vec<_>>();
        let first = empty_table.first().unwrap();
        assert_eq!("", *first);

        // Intern a string literal to ensure ?Sized works.
        table.intern("")?;

        for (offset, str) in cases.iter() {
            let actual_offset = table.intern(str)?;
            assert_eq!(*offset, actual_offset);
        }

        // repeat them to ensure they aren't re-added
        for (offset, str) in cases.iter() {
            let actual_offset = table.intern(str)?;
            assert_eq!(*offset, actual_offset);
        }

        let table_vec = table.iter().collect::<Vec<_>>();
        assert_eq!(cases.len(), table_vec.len());
        let actual = table_vec
            .into_iter()
            .enumerate()
            .map(|(offset, item)| (StringId::from_offset(offset), item))
            .collect::<Vec<_>>();
        assert_eq!(cases, &actual);
        Ok(())
    }
}
