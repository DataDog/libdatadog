// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::SetHasher;
use crate::profiles::ProfileError;
use datadog_profiling_protobuf::{self as pprof, Value};
use std::borrow::Cow;
use std::collections::{hash_map, HashMap};
use std::io::Write;
use std::mem::MaybeUninit;

/// String table that interns strings to compact offsets and encodes into
/// protobuf when a string is added to the map. Uses `Cow<'a, str>` so callers
/// can pass either borrowed strings (zero-copy) or owned strings (e.g. from
/// FFI conversions) without lifetime constraints.
pub struct StringTable<'a> {
    map: HashMap<Cow<'a, str>, pprof::StringOffset, SetHasher>,
}

impl<'a> StringTable<'a> {
    pub fn with_capacity(cap: usize) -> Result<Self, ProfileError> {
        let mut map = HashMap::with_hasher(Default::default());
        map.try_reserve(cap.max(1))?;
        map.insert(Cow::Borrowed(""), pprof::StringOffset::ZERO);
        let this = Self { map };
        Ok(this)
    }

    pub fn emit_existing<W: Write>(&mut self, writer: &mut W) -> Result<(), ProfileError> {
        let mut buffer = {
            let n = self.map.len();
            let mut b = Vec::new();
            b.try_reserve_exact(n)?;
            b.spare_capacity_mut()[0..n].fill(MaybeUninit::new(""));
            // All n items were initialized.
            unsafe { b.set_len(n) };
            b
        };

        for (str, offset) in self.map.iter() {
            let i = usize::from(offset);
            if let Some(slot) = buffer.get_mut(i) {
                *slot = str.as_ref();
            } else {
                return Err(ProfileError::fmt(format_args!(
                    "internal error: attempted to write to out-of-bounds string table index {i}"
                )));
            }
        }

        for item in buffer {
            pprof::Record::<&str, 6, { pprof::NO_OPT_ZERO }>::from(item).encode(writer)?;
        }
        Ok(())
    }

    // Intern a string without writing it to the protobuf output yet.
    pub fn intern_without_write<S: Into<Cow<'a, str>>>(
        &mut self,
        s: S,
    ) -> Result<pprof::StringOffset, ProfileError> {
        self.map.try_reserve(1)?;
        let len = self.map.len();
        match self.map.entry(s.into()) {
            hash_map::Entry::Occupied(o) => Ok(*o.get()),
            hash_map::Entry::Vacant(v) => {
                let current_size = u32::try_from(len).ok().ok_or(ProfileError::other(
                    "pprof string table tried to use more than u32::MAX strings",
                ))?;
                let offset = pprof::StringOffset::from(current_size);
                v.insert(offset);
                Ok(offset)
            }
        }
    }

    pub fn intern<W: Write, S: Into<Cow<'a, str>>>(
        &mut self,
        writer: &mut W,
        s: S,
    ) -> Result<pprof::StringOffset, ProfileError> {
        self.map.try_reserve(1)?;
        let len = self.map.len();
        let cow: Cow<'a, str> = s.into();
        match self.map.entry(cow) {
            hash_map::Entry::Occupied(o) => Ok(*o.get()),
            hash_map::Entry::Vacant(v) => {
                let current_size = u32::try_from(len).ok().ok_or(ProfileError::other(
                    "pprof string table tried to use more than u32::MAX strings",
                ))?;
                let offset = pprof::StringOffset::from(current_size);
                let o = v.insert_entry(offset);
                // Safe: the entry key lives in the map; use its &str view for encoding.
                let to_write: &str = o.key().as_ref();
                pprof::Record::<&str, 6, { pprof::NO_OPT_ZERO }>::from(to_write).encode(writer)?;
                Ok(offset)
            }
        }
    }
}
