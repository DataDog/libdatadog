// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use crate::alloc::{AllocError, ArenaAllocator};
use crate::collections::identifiable::{Id, StringId};
use core::borrow::Borrow;
use core::ops::Deref;
use core::{fmt, hash, mem, ptr};
use std::alloc::{Layout, LayoutError};

type FxHashMap<K, V> =
    std::collections::HashMap<K, V, hash::BuildHasherDefault<rustc_hash::FxHasher>>;

// Not pub, used to do unsafe things.
#[repr(C)]
struct LengthPrefixedHeader {
    // Avoids u16/u32/u64/usize so alignment is always 1.
    len: [u8; mem::size_of::<u16>()],
    // Intentionally missing the [u8] bytes because dynamically sized types
    // are a major pain in stable rust. Use thin ptrs with unsafe instead.
}

impl LengthPrefixedHeader {
    const fn new() -> Self {
        Self {
            len: [0; mem::size_of::<u16>()],
        }
    }
}

/// Dangerous type, has a lifetime which has been elided!
#[derive(Clone, Copy)]
pub(crate) struct LengthPrefixedStr {
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
    // private, these are supposed to only be created by string tables.
    fn new() -> Self {
        static EMPTY: LengthPrefixedHeader = LengthPrefixedHeader::new();
        let header = ptr::NonNull::from(&EMPTY);
        Self { header }
    }

    pub(crate) unsafe fn to_str_with_arena_lifetime<'a>(
        self,
        _string_table: &'a StringTable,
    ) -> &'a str {
        mem::transmute::<&str, &'a str>(self.deref())
    }

    fn layout_of(n: u16) -> Result<Layout, LayoutError> {
        let header = Layout::new::<LengthPrefixedHeader>();
        let array = Layout::array::<u8>(n as usize)?;
        let (layout, offset) = header.extend(array)?;
        debug_assert_eq!(offset, mem::size_of::<u16>());
        // no need to pad, everything is alignment of 1
        Ok(layout)
    }

    /// The ptr needs to point to a valid length prefixed string.
    unsafe fn from_bytes(ptr: ptr::NonNull<u8>) -> Self {
        Self { header: ptr.cast() }
    }

    #[inline]
    unsafe fn from_str_in_mem(s: &str, n: u16, ptr: ptr::NonNull<[u8]>) -> Self {
        // SAFETY: todo
        let header_src = u16::to_be_bytes(n);
        debug_assert!(header_src.len() + s.len() <= ptr.len());

        let header_ptr = ptr.as_ptr() as *mut u8;
        // SAFETY: todo
        ptr::copy_nonoverlapping(header_src.as_ptr(), header_ptr, header_src.len());
        // SAFETY: todo
        let bytes_ptr = header_ptr.add(header_src.len());
        // SAFETY: todo
        ptr::copy_nonoverlapping(s.as_ptr(), bytes_ptr, s.len());
        Self {
            // SAFETY: todo
            header: ptr::NonNull::new_unchecked(header_ptr.cast()),
        }
    }

    fn from_str_in(s: &str, arena: &ArenaAllocator) -> Result<Self, AllocError> {
        let Ok(n) = u16::try_from(s.len()) else {
            return Err(AllocError {});
        };
        let Ok(layout) = Self::layout_of(n) else {
            return Err(AllocError {});
        };
        let ptr = arena.allocate_zeroed(layout)?;
        // SAFETY: todo
        Ok(unsafe { Self::from_str_in_mem(s, n, ptr) })
    }
}

impl Deref for LengthPrefixedStr {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        let header = self.header.as_ptr();
        // SAFETY: todo
        let len = u16::from_be_bytes(unsafe { &*header }.len) as usize;
        let ptr = self.header.as_ptr() as *const u8;
        // SAFETY: todo
        let data = unsafe { ptr.add(mem::size_of::<u16>()) };
        // SAFETY: todo
        let slice = unsafe { core::slice::from_raw_parts(data, len) };
        // SAFETY: todo
        unsafe { core::str::from_utf8_unchecked(slice) }
    }
}

pub struct StringTable {
    arena: ArenaAllocator,
    map: FxHashMap<LengthPrefixedStr, StringId>,
}

#[derive(Debug)]
pub enum InternError {
    LargeString(usize),
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

impl StringTable {
    pub fn with_arena_capacity(min_capacity: usize) -> anyhow::Result<Self> {
        let arena = ArenaAllocator::with_capacity(min_capacity.next_power_of_two())?;
        let mut map = FxHashMap::default();
        if let Err(_err) = map.try_reserve(1) {
            anyhow::bail!("failed to acquire memory for string table's hashmap");
        }
        // Always have the empty string. Note that it doesn't need storage in
        // the arena as it points to static storage.
        map.insert(LengthPrefixedStr::new(), StringId::ZERO);
        Ok(Self { arena, map })
    }

    #[inline(never)]
    pub(crate) fn insert_full<S>(
        &mut self,
        s: &S,
    ) -> Result<(LengthPrefixedStr, StringId), InternError>
    where
        S: ?Sized + Borrow<str>,
    {
        self.intern_inner(s)
    }

    pub(crate) fn intern_inner<S>(
        &mut self,
        s: &S,
    ) -> Result<(LengthPrefixedStr, StringId), InternError>
    where
        S: ?Sized + Borrow<str>,
    {
        let str = s.borrow();
        match self.map.get_key_value(str) {
            None => {
                if let Err(_err) = u16::try_from(str.len()) {
                    return Err(InternError::LargeString(str.len()));
                }
                if let Err(_err) = self.map.try_reserve(1) {
                    return Err(InternError::AllocError);
                }

                let string_id = StringId::from_offset(self.map.len());
                let str = match LengthPrefixedStr::from_str_in(str, &self.arena) {
                    Ok(s) => s,
                    Err(_err) => return Err(InternError::AllocError),
                };
                self.map.insert(str, string_id);
                Ok((str, string_id))
            }
            Some((str, string_id)) => Ok((*str, *string_id)),
        }
    }

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

    pub fn iter(&self) -> StringTableIter {
        StringTableIter {
            arena: &self.arena,
            offset: 0,
            len: self.map.len(),
            has_empty_str: true,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    #[inline]
    pub fn arena_capacity(&self) -> usize {
        match self.arena.mapping.as_ref() {
            None => 0,
            Some(mapping) => mapping.len(),
        }
    }
}

pub struct StringTableIter<'a> {
    arena: &'a ArenaAllocator,
    offset: usize,
    len: usize,
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

        let str = LengthPrefixedStr::from_str_in("datadog", &arena)?;

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
