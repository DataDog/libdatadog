// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::SetHasher;
use crate::profiles::ProfileError;
use datadog_profiling_protobuf::{self as pprof, Value};
use indexmap::map::{Entry, IndexMap};
use indexmap::TryReserveError;
use std::borrow::Cow;
use std::collections::hash_map::{self, HashMap};
use std::io::Write;
use std::num::TryFromIntError;

/// String table that interns strings to compact offsets and encodes into
/// protobuf when a string is added to the map. Uses `Cow<'a, str>` so callers
/// can pass either borrowed strings (zero-copy) or owned strings (e.g. from
/// FFI conversions) without lifetime constraints.
pub struct StringTable<'a> {
    // Using a Map with () as the value for the sake of using the entry API,
    // which is absent on the Set version.
    map: IndexMap<Cow<'a, str>, (), SetHasher>,
}

pub enum StringTableInternError {
    StorageFull(TryFromIntError),
    TryReserveError(TryReserveError),
}

impl From<TryFromIntError> for StringTableInternError {
    fn from(err: TryFromIntError) -> Self {
        StringTableInternError::StorageFull(err)
    }
}

impl From<TryReserveError> for StringTableInternError {
    fn from(err: TryReserveError) -> Self {
        StringTableInternError::TryReserveError(err)
    }
}

impl<'a> StringTable<'a> {
    pub fn with_capacity(cap: usize) -> Result<Self, TryReserveError> {
        let mut map = IndexMap::with_hasher(SetHasher::default());
        map.try_reserve(cap.max(1))?;
        map.insert(Cow::Borrowed(""), ());
        let this = Self { map };
        Ok(this)
    }

    /// Intern a string without writing it to the protobuf output yet.
    pub fn intern<S: Into<Cow<'a, str>>>(
        &mut self,
        s: S,
    ) -> Result<pprof::StringOffset, StringTableInternError> {
        self.map.try_reserve(1)?;
        let len = self.map.len();
        let cow: Cow<'a, str> = s.into();
        let off = match self.map.entry(cow) {
            Entry::Occupied(o) => {
                let result = pprof::StringOffset::try_from(o.index());
                // SAFETY: if it's already interned it cannot be too large.
                unsafe { result.unwrap_unchecked() }
            }
            Entry::Vacant(v) => {
                let next_size = u32::try_from(len.wrapping_add(1))?;
                let offset = pprof::StringOffset::from(next_size.wrapping_sub(1));
                // Insert and gain access to the stable &str key for encoding without cloning.
                let _o = v.insert_entry(());
                debug_assert_eq!(len, _o.index());

                offset
            }
        };
        Ok(off)
    }
}

pub struct StringTableWriter<'a> {
    map: HashMap<Cow<'a, str>, pprof::StringOffset, SetHasher>,
}

impl<'a> StringTableWriter<'a> {
    pub fn from_string_table<W: Write>(
        writer: &mut W,
        table: &mut StringTable<'a>,
    ) -> Result<Self, ProfileError> {
        let mut this = Self {
            map: HashMap::with_hasher(SetHasher::default()),
        };
        this.map.try_reserve(table.map.len()).map_err(|_| {
            ProfileError::other("out of memory: failed to create string table writer")
        })?;
        for (str, _) in table.map.drain(..) {
            this.intern(writer, str)?;
        }
        Ok(this)
    }

    pub fn intern<W: Write, S: Into<Cow<'a, str>>>(
        &mut self,
        writer: &mut W,
        str: S,
    ) -> Result<pprof::StringOffset, ProfileError> {
        self.map
            .try_reserve(1)
            .map_err(|_| ProfileError::other("out of memory: failed to intern string"))?;
        let len = self.map.len();
        let cow: Cow<'a, str> = str.into();
        let off = match self.map.entry(cow) {
            hash_map::Entry::Occupied(o) => *o.get(),
            hash_map::Entry::Vacant(v) => {
                let next_size =
                    u32::try_from(len.wrapping_add(1))
                        .ok()
                        .ok_or(ProfileError::other(
                            "storage full: tried to intern more than u32::MAX strings",
                        ))?;
                let offset = pprof::StringOffset::from(next_size.wrapping_sub(1));
                let o = v.insert_entry(offset);
                pprof::Record::<&str, 6, { pprof::NO_OPT_ZERO }>::from(o.key().as_ref())
                    .encode(writer)?;
                offset
            }
        };
        Ok(off)
    }
}
