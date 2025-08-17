// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Streaming pprof builder that compacts IDs (1..=N) and emits only data
//! that is referenced by added samples across one or more input
//! profiles.
//!
//! Notes:
//! - The pprof string table is separate from internal string sets. We intern and emit strings on
//!   first use into the protobuf string table.
//! - Our stack abstractions (thin slices of `SetId<Location>`) do not exist in pprof; we expand
//!   stacks into `Sample.location_id` arrays as they are encountered while streaming.

use std::collections::{hash_map, HashMap};
use std::io::Write;

use crate::profiles::collections::SetId;
use crate::profiles::datatypes::{Profile, ProfilesDictionary, ScratchPad};
use crate::profiles::ProfileError;
use crate::profiles::{datatypes as dt, datatypes};
use datadog_profiling_protobuf as pprof;
use datadog_profiling_protobuf::Value;

/// Capacity hints to minimize reallocations during interning and
/// id-compaction.
#[derive(Clone, Copy, Debug)]
pub struct PprofOptions {
    pub reserve_strings: usize,
    pub reserve_functions: usize,
    pub reserve_mappings: usize,
    pub reserve_locations: usize,
    pub reserve_samples: usize,
}

impl Default for PprofOptions {
    fn default() -> Self {
        Self {
            reserve_strings: 256,
            reserve_functions: 128,
            reserve_mappings: 32,
            reserve_locations: 1024,
            reserve_samples: 4096,
        }
    }
}

/// Builder that collects multiple inputs and streams an uncompressed
/// pprof to any writer. IDs for mappings/functions/locations are
/// compacted to 1..=N and emitted once per first use; the default id 0
/// entries are not emitted (except the empty string at
/// string_table[0]).
pub struct PprofBuilder<'a> {
    options: PprofOptions,
    dictionary: &'a ProfilesDictionary,
    scratchpad: &'a ScratchPad,
    profiles: Vec<&'a Profile>,
}

impl<'a> PprofBuilder<'a> {
    /// Create a new builder bound to a shared dictionary and scratchpad
    /// that all added profiles will reference.
    pub fn new(dictionary: &'a ProfilesDictionary, scratchpad: &'a ScratchPad) -> Self {
        Self {
            options: PprofOptions::default(),
            dictionary,
            scratchpad,
            profiles: Vec::new(),
        }
    }

    pub fn with_options(mut self, options: PprofOptions) -> Self {
        self.options = options;
        self
    }

    /// Register one more profile to be included in the next build.
    /// Performs a fallible capacity check to avoid panicking on
    /// allocation failure.
    pub fn try_add_profile(&mut self, profile: &'a Profile) -> Result<(), ProfileError> {
        self.profiles.try_reserve(1)?;
        self.profiles.push(profile);
        Ok(())
    }

    /// Produce a complete uncompressed pprof for all added profiles.
    /// Strategy:
    /// - Pass 1: union sample types across all inputs; compute global ordering
    /// - Pass 2: stream strings/functions/mappings/locations/samples on first use
    /// - Defaults (id 0): reserved, not emitted; empty string at offset 0 is emitted
    pub fn build<'b, W: Write>(&'b self, writer: &mut W) -> Result<(), ProfileError>
    where
        'a: 'b,
    {
        // --- string table ---
        let mut string_offsets: HashMap<&'a str, pprof::StringOffset> =
            HashMap::with_capacity(self.options.reserve_strings);
        let mut next_str_off: u32 = 0;

        // emit empty string at offset 0
        pprof::Record::<&str, 6, { pprof::OPT_ZERO }>::from("").encode(writer)?;
        string_offsets.insert("", pprof::StringOffset::from(next_str_off));
        next_str_off = next_str_off
            .checked_add(1)
            .ok_or(ProfileError::StorageFull)?;

        // Helper to intern and emit a string immediately.
        fn intern_string<'a, W: Write>(
            writer: &mut W,
            s: &'a str,
            string_offsets: &mut HashMap<&'a str, pprof::StringOffset>,
            next_str_off: &mut u32,
        ) -> Result<pprof::StringOffset, ProfileError> {
            // Reserve first (which should hit the hot path most of the time),
            // so we can use the entry API to avoid double hashing.
            string_offsets.try_reserve(1)?;
            match string_offsets.entry(s) {
                hash_map::Entry::Occupied(o) => Ok(*o.get()),
                hash_map::Entry::Vacant(v) => {
                    let off = pprof::StringOffset::from(*next_str_off);
                    *next_str_off = next_str_off
                        .checked_add(1)
                        .ok_or(ProfileError::StorageFull)?;
                    v.insert(off);
                    pprof::Record::<&str, 6, { pprof::OPT_ZERO }>::from(s).encode(writer)?;
                    Ok(off)
                }
            }
        }

        // --- compact ids and first-use emission maps ---
        let dict = self.dictionary;
        let scratch = self.scratchpad;
        let mut func_ids: HashMap<SetId<dt::Function>, u64> =
            HashMap::with_capacity(self.options.reserve_functions);
        let mut map_ids: HashMap<SetId<dt::Mapping>, u64> =
            HashMap::with_capacity(self.options.reserve_mappings);
        let mut loc_ids: HashMap<SetId<dt::Location>, u64> =
            HashMap::with_capacity(self.options.reserve_locations);

        // These are incremented before being used, so the first ids are 1.
        let mut next_func_id: u64 = 0;
        let mut next_map_id: u64 = 0;
        let mut next_loc_id: u64 = 0;

        // --- unify sample types across profiles and emit ---
        let mut sample_type_index: HashMap<(&'a str, &'a str), usize> = HashMap::new();
        let mut global_sample_types: Vec<(pprof::StringOffset, pprof::StringOffset)> = Vec::new();
        let mut remaps: Vec<Vec<usize>> = Vec::with_capacity(self.profiles.len());
        for prof in &self.profiles {
            let mut remap = Vec::new();
            for vt in &prof.sample_type {
                let t: &'a str = unsafe { dict.strings().get(vt.type_id) };
                let u: &'a str = unsafe { dict.strings().get(vt.unit_id) };
                let idx = if let Some(i) = sample_type_index.get(&(t, u)).copied() {
                    i
                } else {
                    let toff = intern_string(writer, t, &mut string_offsets, &mut next_str_off)?;
                    let uoff = intern_string(writer, u, &mut string_offsets, &mut next_str_off)?;
                    let i = global_sample_types.len();
                    global_sample_types.push((toff, uoff));
                    sample_type_index.try_reserve(1)?;
                    sample_type_index.insert((t, u), i);
                    i
                };
                remap.push(idx);
            }
            remaps.push(remap);
        }
        for (t_off, u_off) in &global_sample_types {
            let v = pprof::ValueType {
                r#type: pprof::Record::from(*t_off),
                unit: pprof::Record::from(*u_off),
            };
            pprof::Record::<pprof::ValueType, 1, { pprof::OPT_ZERO }>::from(v).encode(writer)?;
        }

        // --- emit helpers ---
        fn ensure_function<'a, W: Write>(
            w: &mut W,
            sid: SetId<dt::Function>,
            dict: &'a ProfilesDictionary,
            next_str_off: &mut u32,
            next_func_id: &mut u64,
            string_offsets: &mut HashMap<&'a str, pprof::StringOffset>,
            func_ids: &mut HashMap<SetId<dt::Function>, u64>,
        ) -> Result<u64, ProfileError> {
            func_ids.try_reserve(1)?;
            match func_ids.entry(sid) {
                hash_map::Entry::Occupied(o) => Ok(*o.get()),
                hash_map::Entry::Vacant(v) => {
                    let id = next_func_id
                        .checked_add(1)
                        .ok_or(ProfileError::StorageFull)?;
                    *next_func_id = id;
                    let dict_strings = dict.strings();
                    let f = unsafe { dict.functions().get(sid) };
                    let name = intern_string(
                        w,
                        unsafe { dict_strings.get(f.name) },
                        string_offsets,
                        next_str_off,
                    )?;
                    let sys = intern_string(
                        w,
                        unsafe { dict_strings.get(f.system_name) },
                        string_offsets,
                        next_str_off,
                    )?;
                    let file = intern_string(
                        w,
                        unsafe { dict_strings.get(f.file_name) },
                        string_offsets,
                        next_str_off,
                    )?;
                    let msg = pprof::Function {
                        id: pprof::Record::from(id),
                        name: pprof::Record::from(name),
                        system_name: pprof::Record::from(sys),
                        filename: pprof::Record::from(file),
                    };
                    pprof::Record::<pprof::Function, 5, { pprof::NO_OPT_ZERO }>::from(msg)
                        .encode(w)?;
                    v.insert(id);
                    Ok(id)
                }
            }
        }

        fn ensure_mapping<'a, W: Write>(
            w: &mut W,
            sid: SetId<dt::Mapping>,
            dict: &'a ProfilesDictionary,
            next_str_off: &mut u32,
            next_map_id: &mut u64,
            string_offsets: &mut HashMap<&'a str, pprof::StringOffset>,
            map_ids: &mut HashMap<SetId<dt::Mapping>, u64>,
        ) -> Result<u64, ProfileError> {
            map_ids.try_reserve(1)?;
            match map_ids.entry(sid) {
                hash_map::Entry::Occupied(o) => Ok(*o.get()),
                hash_map::Entry::Vacant(v) => {
                    let id = next_map_id
                        .checked_add(1)
                        .ok_or(ProfileError::StorageFull)?;
                    *next_map_id = id;
                    let m = unsafe { dict.mappings().get(sid) };
                    let filename = intern_string(
                        w,
                        unsafe { dict.strings().get(m.filename) },
                        string_offsets,
                        next_str_off,
                    )?;
                    let build_id = intern_string(
                        w,
                        unsafe { dict.strings().get(m.build_id) },
                        string_offsets,
                        next_str_off,
                    )?;
                    let msg = pprof::Mapping {
                        id: pprof::Record::from(id),
                        memory_start: pprof::Record::from(m.memory_start),
                        memory_limit: pprof::Record::from(m.memory_limit),
                        file_offset: pprof::Record::from(m.file_offset),
                        filename: pprof::Record::from(filename),
                        build_id: pprof::Record::from(build_id),
                    };
                    pprof::Record::<pprof::Mapping, 3, { pprof::NO_OPT_ZERO }>::from(msg)
                        .encode(w)?;
                    v.insert(id);
                    Ok(id)
                }
            }
        }

        #[allow(clippy::too_many_arguments)]
        fn ensure_location<'a, W: Write>(
            w: &mut W,
            sid: SetId<dt::Location>,
            scratch: &'a ScratchPad,
            dict: &'a ProfilesDictionary,
            next_str_off: &mut u32,
            next_func_id: &mut u64,
            next_map_id: &mut u64,
            next_loc_id: &mut u64,
            string_offsets: &mut HashMap<&'a str, pprof::StringOffset>,
            func_ids: &mut HashMap<SetId<dt::Function>, u64>,
            map_ids: &mut HashMap<SetId<dt::Mapping>, u64>,
            loc_ids: &mut HashMap<SetId<dt::Location>, u64>,
        ) -> Result<u64, ProfileError> {
            loc_ids.try_reserve(1)?;
            match loc_ids.entry(sid) {
                hash_map::Entry::Occupied(o) => Ok(*o.get()),
                hash_map::Entry::Vacant(v) => {
                    let id = next_loc_id
                        .checked_add(1)
                        .ok_or(ProfileError::StorageFull)?;
                    *next_loc_id = id;
                    let loc = unsafe { scratch.locations().get(sid) };
                    let mapping_id = match loc.mapping_id {
                        Some(mid) => ensure_mapping(
                            w,
                            mid,
                            dict,
                            next_str_off,
                            next_map_id,
                            string_offsets,
                            map_ids,
                        )?,
                        None => 0,
                    };
                    let line = if let Some(fid) = loc.line.function_id {
                        let fid64 = ensure_function(
                            w,
                            fid,
                            dict,
                            next_str_off,
                            next_func_id,
                            string_offsets,
                            func_ids,
                        )?;
                        pprof::Line {
                            function_id: pprof::Record::from(fid64),
                            lineno: pprof::Record::from(loc.line.line_number),
                        }
                    } else {
                        pprof::Line {
                            function_id: pprof::Record::from(0u64),
                            lineno: pprof::Record::from(loc.line.line_number),
                        }
                    };
                    let msg = pprof::Location {
                        id: pprof::Record::from(id),
                        mapping_id: pprof::Record::from(mapping_id),
                        address: pprof::Record::from(loc.address),
                        line: pprof::Record::from(line),
                    };
                    pprof::Record::<pprof::Location, 4, { pprof::NO_OPT_ZERO }>::from(msg)
                        .encode(w)?;
                    v.insert(id);
                    Ok(id)
                }
            }
        }

        // --- emit samples ---
        let total_types = global_sample_types.len();
        let mut values_buf: Vec<i64> = Vec::with_capacity(total_types);
        let mut labels_buf: Vec<pprof::Record<pprof::Label, 3, { pprof::NO_OPT_ZERO }>> =
            Vec::new();
        for (pi, prof) in self.profiles.iter().enumerate() {
            let remap = &remaps[pi];
            for sample in &prof.samples {
                // location ids from stack
                let stack = sample.stack_id.as_slice();
                let mut locs_out: Vec<u64> = Vec::with_capacity(stack.len());
                for &lid in stack {
                    let id64 = ensure_location(
                        writer,
                        lid,
                        scratch,
                        dict,
                        &mut next_str_off,
                        &mut next_func_id,
                        &mut next_map_id,
                        &mut next_loc_id,
                        &mut string_offsets,
                        &mut func_ids,
                        &mut map_ids,
                        &mut loc_ids,
                    )?;
                    locs_out.push(id64);
                }

                // labels from attributes and links/endpoints
                labels_buf.clear();
                if !sample.attributes.is_empty() {
                    labels_buf.try_reserve(sample.attributes.len())?;
                }
                for &aid in &sample.attributes {
                    let kv = unsafe { scratch.attributes().get(aid) };
                    let key_str: &'a str = match &kv.key {
                        std::borrow::Cow::Borrowed(s) => s,
                        std::borrow::Cow::Owned(s) => s.as_str(),
                    };
                    let key_off =
                        intern_string(writer, key_str, &mut string_offsets, &mut next_str_off)?;
                    let mut lbl = pprof::Label {
                        key: pprof::Record::from(key_off),
                        ..Default::default()
                    };
                    match &kv.value {
                        dt::AnyValue::String(s) => {
                            let off = intern_string(
                                writer,
                                s.as_str(),
                                &mut string_offsets,
                                &mut next_str_off,
                            )?;
                            lbl.str = pprof::Record::from(off);
                        }
                        dt::AnyValue::Integer(n) => {
                            lbl.num = pprof::Record::from(*n);
                        }
                    }
                    labels_buf.push(pprof::Record::from(lbl));
                }

                // If the sample has a link, emit local root span id and span id
                if let Some(link_id) = sample.link_id {
                    let link = unsafe { scratch.links().get(link_id) };
                    // local root span id
                    {
                        let key_off = intern_string(
                            writer,
                            "local root span id",
                            &mut string_offsets,
                            &mut next_str_off,
                        )?;
                        let lbl = pprof::Label {
                            key: pprof::Record::from(key_off),
                            num: pprof::Record::from(link.local_root_span_id as i64),
                            ..Default::default()
                        };
                        labels_buf.push(pprof::Record::from(lbl));
                    }
                    // span id
                    {
                        let key_off = intern_string(
                            writer,
                            "span id",
                            &mut string_offsets,
                            &mut next_str_off,
                        )?;
                        let lbl = pprof::Label {
                            key: pprof::Record::from(key_off),
                            num: pprof::Record::from(link.span_id as i64),
                            ..Default::default()
                        };
                        labels_buf.push(pprof::Record::from(lbl));
                    }

                    // Add an endpoint if we have it.
                    let lrs_id = link.local_root_span_id as i64;
                    if let Some(endpoint_str) =
                        scratch.endpoint_tracker().get_trace_endpoint_str(lrs_id)
                    {
                        let val_off = intern_string(
                            writer,
                            endpoint_str,
                            &mut string_offsets,
                            &mut next_str_off,
                        )?;
                        let key_off = intern_string(
                            writer,
                            "trace endpoint",
                            &mut string_offsets,
                            &mut next_str_off,
                        )?;
                        let lbl = pprof::Label {
                            key: pprof::Record::from(key_off),
                            str: pprof::Record::from(val_off),
                            ..Default::default()
                        };
                        labels_buf.push(pprof::Record::from(lbl));
                    }
                }

                // align values to global types
                values_buf.clear();
                values_buf.try_reserve(total_types)?;
                values_buf.resize(total_types, 0);
                for (local_idx, &global_idx) in remap.iter().enumerate() {
                    if let Some(v) = sample.values.get(local_idx) {
                        values_buf[global_idx] = *v;
                    }
                }

                let s_msg = pprof::Sample {
                    location_ids: pprof::Record::from(locs_out.as_slice()),
                    values: pprof::Record::from(values_buf.as_slice()),
                    labels: labels_buf.as_slice(),
                };
                pprof::Record::<pprof::Sample, 2, { pprof::NO_OPT_ZERO }>::from(s_msg)
                    .encode(writer)?;
            }
        }

        Ok(())
    }
}
