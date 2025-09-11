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

mod remapper;
mod string_table;
mod upscaling;

use std::borrow::Cow;
pub use string_table::*;
pub use upscaling::*;

use crate::profiles::collections::{SetHasher, SetId, StringId};
use crate::profiles::datatypes::{
    self as dt, Profile, ProfilesDictionary, ScratchPad, MAX_SAMPLE_TYPES,
};
use crate::profiles::ProfileError;
use arrayvec::ArrayVec;
use datadog_profiling_protobuf::{self as pprof, Value};
use ddcommon::error::FfiSafeErrorMessage;
use ddcommon::vec::VecExt;
use std::collections::{hash_map, HashMap};
use std::ffi::CStr;
use std::io::Write;
use std::ptr::NonNull;
use std::time::{SystemTime, UNIX_EPOCH};

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
    dictionary: &'a ProfilesDictionary,
    scratchpad: &'a ScratchPad,
    state: PprofBuilderState<'a>,
}

enum PprofBuilderState<'a> {
    Initialized,
    Configured {
        options: PprofOptions,
    },
    AddingProfiles {
        options: PprofOptions,
        profiles: Vec<(&'a Profile, Vec<UpscalingRule>)>,
        string_table: StringTable<'a>,
    },
}
use crate::profiles::pprof_builder::remapper::Remapper;
use PprofBuilderState::*;

#[derive(Debug)]
pub enum TryAddProfileError {
    InternalError,
    StorageFullLabelIntern,
    OutOfMemoryLabelIntern,
    OutOfMemoryPoissonUpscaling,
    OutOfMemoryProportionalUpscaling,
    OutOfMemoryStringTableNew,
    OutOfMemoryWithoutUpscaling,
    WrongSampleTypeCountForPoisson,

    // Make sure this is an FFI-safe CStr!
    Other(&'static CStr),
}

// SAFETY: we ensure c-str literals here or i
unsafe impl FfiSafeErrorMessage for TryAddProfileError {
    fn as_ffi_str(&self) -> &'static CStr {
        match self {
            TryAddProfileError::InternalError => c"internal error: pprof builder is in an invalid state",
            TryAddProfileError::StorageFullLabelIntern => c"storage full: pprof builder couldn't intern a label string because the string table was full",
            TryAddProfileError::OutOfMemoryLabelIntern => c"out of memory: pprof builder couldn't intern a label string",
            TryAddProfileError::OutOfMemoryPoissonUpscaling => c"out of memory: pprof builder couldn't allocate a new profile with poisson upscaling",
            TryAddProfileError::OutOfMemoryWithoutUpscaling => c"out of memory: pprof builder couldn't allocate a new profile without upscaling rules",
            TryAddProfileError::OutOfMemoryProportionalUpscaling => c"out of memory: pprof builder couldn't allocate a new profile with proportional upscaling",
            TryAddProfileError::OutOfMemoryStringTableNew => c"out of memory: pprof builder couldn't allocate a new string table",
            TryAddProfileError::WrongSampleTypeCountForPoisson => c"invalid input: profile's sample type count must be at least 2 for poisson upscaling",
            TryAddProfileError::Other(e) => e,
        }
    }
}

impl<'a> PprofBuilder<'a> {
    /// Create a new builder bound to a shared dictionary and scratchpad
    /// that all added profiles will reference.
    pub fn new(dictionary: &'a ProfilesDictionary, scratchpad: &'a ScratchPad) -> Self {
        let state = Initialized;
        PprofBuilder {
            dictionary,
            scratchpad,
            state,
        }
    }

    /// Customize the options. Should be done before adding upscaling rules
    /// for profiles.
    pub fn with_options(&mut self, options: PprofOptions) -> Result<(), ProfileError> {
        match self.state {
            Initialized | Configured { .. } => {
                self.state = Configured { options };
                Ok(())
            }
            AddingProfiles { .. } => Err(ProfileError::other(
                "tried to configure pprof builder after configuration state",
            )),
        }
    }

    pub fn try_add_profile_with_proportional_upscaling<I, E>(
        &mut self,
        profile: &'a Profile,
        upscaling_rules: I,
    ) -> Result<(), TryAddProfileError>
    where
        E: FfiSafeErrorMessage,
        I: ExactSizeIterator<Item = Result<((StringId, Cow<'a, str>), f64), E>>,
    {
        let string_set = self.dictionary.strings();
        let (profiles, string_table) = self.transition_to_adding_profiles()?;

        let mut new_rules = Vec::new();
        new_rules
            .try_reserve_exact(upscaling_rules.len())
            .map_err(|_| TryAddProfileError::OutOfMemoryProportionalUpscaling)?;
        for result in upscaling_rules {
            let ((label_key, label_value), scale) =
                result.map_err(|e| TryAddProfileError::Other(e.as_ffi_str()))?;

            // SAFETY: dictionary is supposed to hold all these interned strings.
            let key = string_table
                .intern(unsafe { string_set.get(label_key) })
                .map_err(|e| match e {
                    StringTableInternError::StorageFull(_) => {
                        TryAddProfileError::StorageFullLabelIntern
                    }
                    StringTableInternError::TryReserveError(_) => {
                        TryAddProfileError::OutOfMemoryLabelIntern
                    }
                })?;
            let value = string_table.intern(label_value).map_err(|e| match e {
                StringTableInternError::StorageFull(_) => {
                    TryAddProfileError::StorageFullLabelIntern
                }
                StringTableInternError::TryReserveError(_) => {
                    TryAddProfileError::OutOfMemoryLabelIntern
                }
            })?;
            let group_by_label = GroupByLabel { key, value };
            let rule = ProportionalUpscalingRule {
                group_by_label,
                scale,
            };
            new_rules.push(UpscalingRule::ProportionalUpscalingRule(rule));
        }

        profiles
            .try_push((profile, new_rules))
            .map_err(|_| TryAddProfileError::OutOfMemoryProportionalUpscaling)
    }

    pub fn try_add_profile_with_poisson_upscaling(
        &mut self,
        profile: &'a Profile,
        upscaling_rule: PoissonUpscalingRule,
    ) -> Result<(), TryAddProfileError> {
        let (profiles, _) = self.transition_to_adding_profiles()?;

        if profile.sample_type.len() < 2 {
            return Err(TryAddProfileError::WrongSampleTypeCountForPoisson);
        }

        let mut new_rules = Vec::new();
        new_rules
            .try_reserve_exact(1)
            .map_err(|_| TryAddProfileError::OutOfMemoryPoissonUpscaling)?;
        new_rules.push(UpscalingRule::PoissonUpscalingRule(upscaling_rule));

        profiles
            .try_push((profile, new_rules))
            .map_err(|_| TryAddProfileError::OutOfMemoryPoissonUpscaling)
    }

    pub fn try_add_profile(&mut self, profile: &'a Profile) -> Result<(), TryAddProfileError> {
        let (profiles, _) = self.transition_to_adding_profiles()?;

        profiles
            .try_push((profile, Vec::new()))
            .map_err(|_| TryAddProfileError::OutOfMemoryWithoutUpscaling)
    }

    fn transition_to_adding_profiles(
        &mut self,
    ) -> Result<
        (
            &mut Vec<(&'a Profile, Vec<UpscalingRule>)>,
            &mut StringTable<'a>,
        ),
        TryAddProfileError,
    > {
        if matches!(self.state, Initialized) {
            let options = PprofOptions::default();
            self.state = Configured { options };
        }

        if let Configured { options } = &mut self.state {
            self.state = AddingProfiles {
                options: *options,
                profiles: Vec::new(),
                string_table: StringTable::with_capacity(options.reserve_strings)
                    .map_err(|_| TryAddProfileError::OutOfMemoryStringTableNew)?,
            };
        }

        let AddingProfiles {
            profiles,
            string_table,
            ..
        } = &mut self.state
        else {
            // This should be unreachable. The intent is that that all
            // previous states can be forwarded to adding profiles, and the
            // above code is supposed to do that.
            return Err(TryAddProfileError::InternalError);
        };
        Ok((profiles, string_table))
    }

    /// Produce a complete uncompressed pprof for all added profiles.
    /// todo: document strategy
    pub fn build<W: Write>(&mut self, writer: &mut W) -> Result<(), ProfileError> {
        let AddingProfiles {
            options,
            profiles,
            string_table,
        } = &mut self.state
        else {
            return Err(ProfileError::other(
                "internal error: tried to build a pprof without adding any profiles",
            ));
        };

        let mut string_table = StringTableWriter::from_string_table(writer, string_table)?;

        // --- compact ids and first-use emission maps ---
        let dict = &mut self.dictionary;
        let scratch = &mut self.scratchpad;
        let mut func_ids: CompactIdMap<SetId<dt::Function>> =
            CompactIdMap::with_capacity(options.reserve_functions);
        let mut map_ids: CompactIdMap<SetId<dt::Mapping>> =
            CompactIdMap::with_capacity(options.reserve_mappings);
        let mut loc_ids: CompactIdMap<SetId<dt::Location>> =
            CompactIdMap::with_capacity(options.reserve_locations);

        let dict_strings = dict.strings();

        // --- unify sample types across profiles and emit ---
        let n_sample_types = profiles.iter().map(|(p, _)| p.sample_type.len()).sum();
        let n_profiles = profiles.len();

        let mut remaps: Vec<ArrayVec<usize, MAX_SAMPLE_TYPES>> = Vec::new();
        {
            let mut remapper = Remapper::new(dict_strings, &mut string_table, n_sample_types)?;
            remaps.try_reserve_exact(n_profiles)?;
            for profile in profiles.iter().map(|(p, _)| p) {
                let mut offsets = ArrayVec::new();
                for sample_type in profile.sample_type.iter() {
                    if offsets
                        .try_push(remapper.remap(writer, *sample_type)?)
                        .is_err()
                    {
                        return Err(ProfileError::other("internal error: pprof builder had mismatched capacities for sample types"));
                    }
                }
                if remaps.try_push(offsets).is_err() {
                    return Err(ProfileError::other(
                        "out of memory: pprof builder couldn't remap sample types",
                    ));
                }
            }
        }

        // --- emit samples ---
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
        let mut values_buf: Vec<i64> = Vec::new();
        let mut labels_buf: Vec<pprof::Record<pprof::Label, 3, { pprof::NO_OPT_ZERO }>> =
            Vec::new();
        for (i, (prof, upscaling_rules)) in profiles.iter().enumerate() {
            let remap = &remaps[i];
            for sample in &prof.samples {
                // location ids from stack
                let stack = sample.stack_id.as_slice();
                let mut locs_out: Vec<u64> = Vec::with_capacity(stack.len());
                for &lid in stack {
                    let id64 = Self::ensure_location(
                        writer,
                        lid.cast(),
                        scratch,
                        dict,
                        &mut string_table,
                        &mut func_ids,
                        &mut map_ids,
                        &mut loc_ids,
                    )?;
                    locs_out.push(id64);
                }

                // labels from attributes and links/endpoints
                labels_buf.clear();
                let mut n_labels = sample.attributes.len();

                // 2 for "local root span id", "span id"; calculate + 1
                // for "trace endpoint" is calculated in the body.
                n_labels +=
                    (sample.link_id.is_some() as usize) * 2 + (sample.timestamp.is_some() as usize);
                // If the sample has a link, emit local root span id, span id,
                // and optional endpoint info.
                if let Some(link_id) = sample.link_id {
                    let link = unsafe { scratch.links().get(link_id) };

                    // Add an endpoint if we have it.
                    let lrs_id = link.local_root_span_id as i64;
                    let endpoint_str = if let Some(endpoint_str) =
                        scratch.endpoint_tracker().get_trace_endpoint_str(lrs_id)
                    {
                        n_labels += 1;
                        endpoint_str
                    } else {
                        ""
                    };

                    labels_buf.try_reserve_exact(n_labels)?;

                    if !endpoint_str.is_empty() {
                        let val_off = string_table.intern(writer, endpoint_str)?;
                        let key_off = string_table.intern(writer, "trace endpoint")?;
                        let lbl = pprof::Label {
                            key: pprof::Record::from(key_off),
                            str: pprof::Record::from(val_off),
                            ..Default::default()
                        };
                        labels_buf.push(pprof::Record::from(lbl));
                    }
                    // local root span id
                    {
                        let key_off = string_table.intern(writer, "local root span id")?;
                        let lbl = pprof::Label {
                            key: pprof::Record::from(key_off),
                            num: pprof::Record::from(link.local_root_span_id as i64),
                            ..Default::default()
                        };
                        labels_buf.push(pprof::Record::from(lbl));
                    }
                    // span id
                    {
                        let key_off = string_table.intern(writer, "span id")?;
                        let lbl = pprof::Label {
                            key: pprof::Record::from(key_off),
                            num: pprof::Record::from(link.span_id as i64),
                            ..Default::default()
                        };
                        labels_buf.push(pprof::Record::from(lbl));
                    }
                } else {
                    labels_buf.try_reserve_exact(n_labels)?;
                }
                for &aid in &sample.attributes {
                    let kv = unsafe { scratch.attributes().get(aid) };
                    let key_str: &'a str = unsafe { dict_strings.get(kv.key) };
                    let key_off = string_table.intern(writer, key_str)?;
                    let mut lbl = pprof::Label {
                        key: pprof::Record::from(key_off),
                        ..Default::default()
                    };
                    match &kv.value {
                        dt::AnyValue::String(s) => {
                            let off = string_table.intern(writer, s.as_str())?;
                            lbl.str = pprof::Record::from(off);
                        }
                        dt::AnyValue::Integer(n) => {
                            lbl.num = pprof::Record::from(*n);
                        }
                    }
                    labels_buf.push(pprof::Record::from(lbl));
                }

                // align values to global types
                values_buf.clear();
                values_buf.try_reserve(n_sample_types)?;
                values_buf.resize(n_sample_types, 0);
                for (local_idx, &global_idx) in remap.iter().enumerate() {
                    if let Some(v) = sample.values.get(local_idx) {
                        values_buf[global_idx] = *v;
                    }
                }

                // Optional: emit end_timestamp_ns if present on the sample.
                // Errors in allocation and converting to nanoseconds
                // since the Unix epoch cause the label to be skipped.
                if let Some(ts) = sample.timestamp {
                    let key = string_table.intern(writer, "end_timestamp_ns")?;
                    Self::add_sample_timestamp_label(&mut labels_buf, ts, key);
                }

                // Apply upscaling
                for rule in upscaling_rules {
                    rule.scale(&mut values_buf, &labels_buf);
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
        labels_buf: &mut Vec<pprof::Record<pprof::Label, 3, { pprof::NO_OPT_ZERO }>>,
        ts: SystemTime,
        key: pprof::StringOffset,
    ) {
        // already reserved memory, see  `n_labels`
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

    fn ensure_function<W: Write>(
        w: &mut W,
        sid: SetId<dt::Function>,
        dict: &'a ProfilesDictionary,
        strings: &mut StringTableWriter<'a>,
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

    fn ensure_mapping<W: Write>(
        w: &mut W,
        sid: SetId<dt::Mapping>,
        dict: &'a ProfilesDictionary,
        strings: &mut StringTableWriter<'a>,
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
    fn ensure_location<W: Write>(
        w: &mut W,
        sid: SetId<dt::Location>,
        scratch: &'a ScratchPad,
        dict: &'a ProfilesDictionary,
        strings: &mut StringTableWriter<'a>,
        func_ids: &mut CompactIdMap<SetId<dt::Function>>,
        map_ids: &mut CompactIdMap<SetId<dt::Mapping>>,
        loc_ids: &mut CompactIdMap<SetId<dt::Location>>,
    ) -> Result<u64, ProfileError> {
        loc_ids.ensure_with(sid, |id| {
            let loc = unsafe { scratch.locations().get(sid) };
            let mapping_id = match NonNull::new(loc.mapping_id) {
                Some(mid) => {
                    let set_id = unsafe { SetId::from_raw(mid.cast()) };
                    Self::ensure_mapping(w, set_id, dict, strings, map_ids)?
                }
                None => 0,
            };
            let line = if let Some(fid) = NonNull::new(loc.line.function_id) {
                let function_id = unsafe { SetId::from_raw(fid) };
                let fid64 = Self::ensure_function(w, function_id, dict, strings, func_ids)?;
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
}
