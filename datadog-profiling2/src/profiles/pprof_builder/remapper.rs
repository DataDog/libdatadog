// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::ParallelStringSet;
use crate::profiles::{datatypes as dt, ProfileError, StringTableWriter};
use datadog_profiling_protobuf as pprof;
use datadog_profiling_protobuf::Value;
use hashbrown::hash_map::Entry;
use hashbrown::HashMap;
use std::io::Write;

pub struct Remapper<'a: 'b, 'b> {
    string_set: &'a ParallelStringSet,
    string_table: &'b mut StringTableWriter<'a>,
    association:
        HashMap<dt::ValueType2, usize, core::hash::BuildHasherDefault<rustc_hash::FxHasher>>,
    sample_types: Vec<pprof::ValueType>,
}

impl<'a: 'b, 'b> Remapper<'a, 'b> {
    pub fn new(
        string_set: &'a ParallelStringSet,
        string_table: &'b mut StringTableWriter<'a>,
        n_sample_types: usize,
    ) -> Result<Self, ProfileError> {
        let mut association = HashMap::with_hasher(Default::default());
        association.try_reserve(n_sample_types)?;
        let mut sample_types = Vec::new();
        sample_types.try_reserve_exact(n_sample_types)?;
        Ok(Self {
            string_set,
            string_table,
            association,
            sample_types,
        })
    }

    /// Maps the sample type from specific profile to a pprof::Profile offset.
    /// This will intern value table's strings and emit them, as well as emit
    /// the new sample type.
    pub fn remap<W: Write>(
        &mut self,
        writer: &mut W,
        value_type: dt::ValueType2,
    ) -> Result<usize, ProfileError> {
        let offset = self.association.len();
        let string_set = self.string_set;
        // These _should_ be no-ops, if the size hint in the constructor was
        // correct.
        self.association.try_reserve(1)?;
        self.sample_types.try_reserve(1)?;
        match self.association.entry(value_type) {
            Entry::Occupied(o) => Err(ProfileError::fmt(format_args!(
                "invalid input: {value_type:?} was already seen at index {}",
                o.get()
            ))),
            Entry::Vacant(v) => {
                let t: &str = unsafe { string_set.get(value_type.type_id) };
                let u: &str = unsafe { string_set.get(value_type.unit_id) };
                let toff = self.string_table.intern(writer, t)?;
                let uoff = self.string_table.intern(writer, u)?;
                pprof::Record::<pprof::ValueType, 1, { pprof::NO_OPT_ZERO }>::from(
                    pprof::ValueType {
                        r#type: pprof::Record::from(toff),
                        unit: pprof::Record::from(uoff),
                    },
                )
                .encode(writer)?;
                v.insert(offset);
                Ok(offset)
            }
        }
    }
}
