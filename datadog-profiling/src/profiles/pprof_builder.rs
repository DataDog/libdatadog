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
use std::time::{SystemTime, UNIX_EPOCH};

use crate::profiles::collections::{SetHasher, SetId};
use crate::profiles::datatypes::{self as dt, Profile, ProfilesDictionary, ScratchPad};
use crate::profiles::ProfileError;
use datadog_profiling_protobuf::{self as pprof, Value};

/// String table that interns &str to compact offsets and encodes into
/// protobuf when a string is added to the map.
struct StringTable<'a> {
    map: HashMap<&'a str, pprof::StringOffset, SetHasher>,
    next: u32,
}

impl<'a> StringTable<'a> {
    fn with_capacity(cap: usize) -> Self {
        Self {
            map: HashMap::with_capacity_and_hasher(cap, Default::default()),
            next: 0,
        }
    }

    fn emit_empty<W: Write>(&mut self, writer: &mut W) -> Result<(), ProfileError> {
        pprof::Record::<&str, 6, { pprof::NO_OPT_ZERO }>::from("").encode(writer)?;
        self.map.insert("", pprof::StringOffset::from(self.next));
        self.next += 1;
        Ok(())
    }

    fn intern<W: Write>(
        &mut self,
        writer: &mut W,
        s: &'a str,
    ) -> Result<pprof::StringOffset, ProfileError> {
        self.map.try_reserve(1)?;
        match self.map.entry(s) {
            hash_map::Entry::Occupied(o) => Ok(*o.get()),
            hash_map::Entry::Vacant(v) => {
                let off = pprof::StringOffset::from(self.next);
                self.next = self.next.checked_add(1).ok_or(ProfileError::StorageFull)?;
                v.insert(off);
                pprof::Record::<&str, 6, { pprof::NO_OPT_ZERO }>::from(s).encode(writer)?;
                Ok(off)
            }
        }
    }
}

/// Compacts ids into "offsets" that begin at 1 since id=0 is reserved in
/// pprof for these types. It serializes the K to protobuf when it first gets
/// added to the map.
struct CompactIdMap<K> {
    map: HashMap<K, u64, SetHasher>,
    next: u64,
}

impl<K: Eq + core::hash::Hash> CompactIdMap<K> {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            map: HashMap::with_capacity_and_hasher(capacity, Default::default()),
            next: 0,
        }
    }

    fn ensure_with<F>(&mut self, key: K, mut on_first_use: F) -> Result<u64, ProfileError>
    where
        F: FnMut(u64) -> Result<(), ProfileError>,
    {
        self.map.try_reserve(1)?;
        match self.map.entry(key) {
            hash_map::Entry::Occupied(o) => Ok(*o.get()),
            hash_map::Entry::Vacant(v) => {
                let id = self.next.checked_add(1).ok_or(ProfileError::StorageFull)?;
                self.next = id;
                on_first_use(id)?;
                v.insert(id);
                Ok(id)
            }
        }
    }
}

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
        // These are using 7/8th of a power of 2 because that's the max load
        // factor for hash tables in the current implementation.
        Self {
            reserve_strings: 224,
            reserve_functions: 112,
            reserve_mappings: 0, // some languages don't use these at all
            reserve_locations: 896,
            reserve_samples: 3584,
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
        let mut strings = StringTable::with_capacity(self.options.reserve_strings);
        strings.emit_empty(writer)?;

        // --- compact ids and first-use emission maps ---
        let dict = self.dictionary;
        let scratch = self.scratchpad;
        let mut func_ids: CompactIdMap<SetId<dt::Function>> =
            CompactIdMap::with_capacity(self.options.reserve_functions);
        let mut map_ids: CompactIdMap<SetId<dt::Mapping>> =
            CompactIdMap::with_capacity(self.options.reserve_mappings);
        let mut loc_ids: CompactIdMap<SetId<dt::Location>> =
            CompactIdMap::with_capacity(self.options.reserve_locations);

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
                    let toff = strings.intern(writer, t)?;
                    let uoff = strings.intern(writer, u)?;
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
            pprof::Record::<pprof::ValueType, 1, { pprof::NO_OPT_ZERO }>::from(v).encode(writer)?;
        }

        // --- emit helpers ---
        fn ensure_function<'a, W: Write>(
            w: &mut W,
            sid: SetId<dt::Function>,
            dict: &'a ProfilesDictionary,
            strings: &mut StringTable<'a>,
            func_ids: &mut CompactIdMap<SetId<dt::Function>>,
        ) -> Result<u64, ProfileError> {
            func_ids.ensure_with(sid, |id| {
                let dict_strings = dict.strings();
                let f = unsafe { dict.functions().get(sid) };
                let name = strings.intern(w, unsafe { dict_strings.get(f.name) })?;
                let sys = strings.intern(w, unsafe { dict_strings.get(f.system_name) })?;
                let file = strings.intern(w, unsafe { dict_strings.get(f.file_name) })?;
                let msg = pprof::Function {
                    id: pprof::Record::from(id),
                    name: pprof::Record::from(name),
                    system_name: pprof::Record::from(sys),
                    filename: pprof::Record::from(file),
                };
                pprof::Record::<pprof::Function, 5, { pprof::NO_OPT_ZERO }>::from(msg).encode(w)?;
                Ok(())
            })
        }

        fn ensure_mapping<'a, W: Write>(
            w: &mut W,
            sid: SetId<dt::Mapping>,
            dict: &'a ProfilesDictionary,
            strings: &mut StringTable<'a>,
            map_ids: &mut CompactIdMap<SetId<dt::Mapping>>,
        ) -> Result<u64, ProfileError> {
            map_ids.ensure_with(sid, |id| {
                let m = unsafe { dict.mappings().get(sid) };
                let filename = strings.intern(w, unsafe { dict.strings().get(m.filename) })?;
                let build_id = strings.intern(w, unsafe { dict.strings().get(m.build_id) })?;
                let msg = pprof::Mapping {
                    id: pprof::Record::from(id),
                    memory_start: pprof::Record::from(m.memory_start),
                    memory_limit: pprof::Record::from(m.memory_limit),
                    file_offset: pprof::Record::from(m.file_offset),
                    filename: pprof::Record::from(filename),
                    build_id: pprof::Record::from(build_id),
                };
                pprof::Record::<pprof::Mapping, 3, { pprof::NO_OPT_ZERO }>::from(msg).encode(w)?;
                Ok(())
            })
        }

        #[allow(clippy::too_many_arguments)]
        fn ensure_location<'a, W: Write>(
            w: &mut W,
            sid: SetId<dt::Location>,
            scratch: &'a ScratchPad,
            dict: &'a ProfilesDictionary,
            strings: &mut StringTable<'a>,
            func_ids: &mut CompactIdMap<SetId<dt::Function>>,
            map_ids: &mut CompactIdMap<SetId<dt::Mapping>>,
            loc_ids: &mut CompactIdMap<SetId<dt::Location>>,
        ) -> Result<u64, ProfileError> {
            loc_ids.ensure_with(sid, |id| {
                let loc = unsafe { scratch.locations().get(sid) };
                let mapping_id = match loc.mapping_id {
                    Some(mid) => {
                        let set_id = unsafe { SetId::from_raw(mid.cast()) };
                        ensure_mapping(w, set_id, dict, strings, map_ids)?
                    }
                    None => 0,
                };
                let line = if let Some(fid) = loc.line.function_id {
                    let function_id = unsafe { SetId::from_raw(fid) };
                    let fid64 = ensure_function(w, function_id, dict, strings, func_ids)?;
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
                pprof::Record::<pprof::Location, 4, { pprof::NO_OPT_ZERO }>::from(msg).encode(w)?;
                Ok(())
            })
        }

        // --- emit samples ---
        let total_types = global_sample_types.len();
        let mut values_buf: Vec<i64> = Vec::with_capacity(total_types);
        let mut labels_buf: Vec<pprof::Record<pprof::Label, 3, { pprof::NO_OPT_ZERO }>> =
            Vec::new();
        // Emit profile-level time_nanos and duration_nanos if available from ScratchPad interval
        if let Some((start, end)) = scratch.interval() {
            let start_ns = start
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let end_ns = end
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let duration_ns = end_ns.saturating_sub(start_ns);
            let start_i64 = if start_ns > i64::MAX as u128 {
                i64::MAX
            } else {
                start_ns as i64
            };
            let duration_i64 = if duration_ns > i64::MAX as u128 {
                i64::MAX
            } else {
                duration_ns as i64
            };
            // Field 9: time_nanos, Field 10: duration_nanos in the Profile message
            pprof::Record::<i64, 9, { pprof::OPT_ZERO }>::from(start_i64).encode(writer)?;
            pprof::Record::<i64, 10, { pprof::OPT_ZERO }>::from(duration_i64).encode(writer)?;
        }
        for (pi, prof) in self.profiles.iter().enumerate() {
            let remap = &remaps[pi];
            for sample in &prof.samples {
                // location ids from stack
                let stack = sample.stack_id.as_slice();
                let mut locs_out: Vec<u64> = Vec::with_capacity(stack.len());
                for &lid in stack {
                    let id64 = ensure_location(
                        writer,
                        lid.cast(),
                        scratch,
                        dict,
                        &mut strings,
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
                    let key_str: &'a str = unsafe { dict.strings().get(kv.key) };
                    let key_off = strings.intern(writer, key_str)?;
                    let mut lbl = pprof::Label {
                        key: pprof::Record::from(key_off),
                        ..Default::default()
                    };
                    match &kv.value {
                        dt::AnyValue::String(s) => {
                            let off = strings.intern(writer, s.as_str())?;
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
                        let key_off = strings.intern(writer, "local root span id")?;
                        let lbl = pprof::Label {
                            key: pprof::Record::from(key_off),
                            num: pprof::Record::from(link.local_root_span_id as i64),
                            ..Default::default()
                        };
                        labels_buf.push(pprof::Record::from(lbl));
                    }
                    // span id
                    {
                        let key_off = strings.intern(writer, "span id")?;
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
                        let val_off = strings.intern(writer, endpoint_str)?;
                        let key_off = strings.intern(writer, "trace endpoint")?;
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

                // Optional: emit end_timestamp_ns if present on the sample.
                // Errors in allocation and converting to nanoseconds
                // since the Unix epoch cause the label to be skipped.
                if let Some(ts) = sample.timestamp {
                    let key = strings.intern(writer, "end_timestamp_ns")?;
                    Self::add_sample_timestamp_label(&mut labels_buf, ts, key);
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

    fn add_sample_timestamp_label(
        labels_buf: &mut Vec<pprof::Record<pprof::Label, 3, false>>,
        ts: SystemTime,
        key: pprof::StringOffset,
    ) {
        if labels_buf.try_reserve(1).is_err() {
            return;
        }
        let Ok(dur) = ts.duration_since(UNIX_EPOCH) else {
            return;
        };
        let nanos = dur.as_nanos();
        let total_i64 = if nanos > i64::MAX as u128 {
            i64::MAX
        } else {
            nanos as i64
        };
        let lbl = pprof::Label {
            key: pprof::Record::from(key),
            num: pprof::Record::from(total_i64),
            ..Default::default()
        };
        labels_buf.push(pprof::Record::from(lbl));
    }
}
