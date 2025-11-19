// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod fuzz_tests;

pub mod interning_api;
mod profiles_dictionary_translator;

pub use profiles_dictionary_translator::*;

use self::api::UpscalingInfo;
use super::*;
use crate::api::ManagedStringId;
use crate::api2::{Period2, ValueType2};
use crate::collections::identifiable::*;
use crate::collections::string_storage::{CachedProfileId, ManagedStringStorage};
use crate::collections::string_table::{self, StringTable};
use crate::internal::owned_types;
use crate::iter::{IntoLendingIterator, LendingIterator};
use crate::profiles::collections::ArcOverflow;
use crate::profiles::datatypes::ProfilesDictionary;
use crate::profiles::{Compressor, DefaultProfileCodec};
use crate::{api, api2};
use anyhow::Context;
use interning_api::Generation;
use libdd_profiling_protobuf::{self as protobuf, Record, Value, NO_OPT_ZERO, OPT_ZERO};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::ops::Deref;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

pub struct Profile {
    /// Translates from the new Id2 APIs to the older internal APIs. Long-term,
    /// this should probably use the dictionary directly.
    profiles_dictionary_translator: Option<ProfilesDictionaryTranslator>,
    /// When profiles are reset, the sample-types need to be preserved. This
    /// maintains them in a way that does not depend on the string table. The
    /// Option part is this is taken from the old profile and moved to the new
    /// one.
    owned_sample_types: Option<Box<[owned_types::ValueType]>>,
    /// When profiles are reset, the period needs to be preserved. This
    /// stores it in a way that does not depend on the string table.
    owned_period: Option<owned_types::Period>,
    active_samples: AtomicU64,
    endpoints: Endpoints,
    functions: FxIndexSet<Function>,
    generation: interning_api::Generation,
    labels: FxIndexSet<Label>,
    label_sets: FxIndexSet<LabelSet>,
    locations: FxIndexSet<Location>,
    mappings: FxIndexSet<Mapping>,
    observations: Observations,
    period: Option<(i64, ValueType)>,
    sample_types: Box<[ValueType]>,
    stack_traces: FxIndexSet<StackTrace>,
    start_time: SystemTime,
    strings: StringTable,
    string_storage: Option<Arc<Mutex<ManagedStringStorage>>>,
    string_storage_cached_profile_id: Option<CachedProfileId>,
    timestamp_key: StringId,
    upscaling_rules: UpscalingRules,
}

pub struct EncodedProfile {
    pub start: SystemTime,
    pub end: SystemTime,
    pub buffer: Vec<u8>,
    pub endpoints_stats: ProfiledEndpointsStats,
}

impl EncodedProfile {
    pub fn test_instance() -> anyhow::Result<Self> {
        use std::io::Read;

        fn open<P: AsRef<std::path::Path>>(path: P) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
            let mut file = std::fs::File::open(path)?;
            let metadata = file.metadata()?;
            let mut buffer = Vec::with_capacity(metadata.len() as usize);
            file.read_to_end(&mut buffer)?;

            Ok(buffer)
        }

        let small_pprof_name = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/profile.pprof");
        let buffer = open(small_pprof_name).map_err(|e| anyhow::anyhow!("{e}"))?;
        let start = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_nanos(12000000034))
            .context("Translating time failed")?;
        let end = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_nanos(56000000078))
            .context("Translating time failed")?;
        let endpoints_stats = ProfiledEndpointsStats::default();
        Ok(EncodedProfile {
            start,
            end,
            buffer,
            endpoints_stats,
        })
    }
}

/// Public API
impl Profile {
    /// When testing on some profiles that can't be shared publicly,
    /// level 1 provided better compressed files while taking less or equal
    /// time compared to lz4.
    pub const COMPRESSION_LEVEL: i32 = 1;

    /// Add the endpoint data to the endpoint mappings.
    /// The `endpoint` string will be interned.
    pub fn add_endpoint(
        &mut self,
        local_root_span_id: u64,
        endpoint: Cow<str>,
    ) -> anyhow::Result<()> {
        let interned_endpoint = self.try_intern(endpoint.as_ref())?;

        let mappings = &mut self.endpoints.mappings;
        mappings.try_reserve(1)?;
        mappings.insert(local_root_span_id, interned_endpoint);
        Ok(())
    }

    pub fn add_endpoint_count(&mut self, endpoint: Cow<str>, value: i64) -> anyhow::Result<()> {
        self.endpoints
            .stats
            .add_endpoint_count(endpoint.into_owned(), value);
        Ok(())
    }

    pub fn try_add_sample(
        &mut self,
        sample: api::Sample,
        timestamp: Option<Timestamp>,
    ) -> anyhow::Result<()> {
        #[cfg(debug_assertions)]
        {
            self.validate_sample_labels(&sample)?;
        }

        let labels = {
            let mut lbls = Vec::new();
            lbls.try_reserve_exact(sample.labels.len())?;
            for label in &sample.labels {
                let key = self.try_intern(label.key)?;
                let internal_label = if !label.str.is_empty() {
                    let str = self.try_intern(label.str)?;
                    Label::str(key, str)
                } else {
                    let num = label.num;
                    let num_unit = self.try_intern(label.num_unit)?;
                    Label::num(key, num, num_unit)
                };

                let id = self.labels.try_dedup(internal_label)?;
                lbls.push(id);
            }
            lbls.into_boxed_slice()
        };

        let mut locations = Vec::new();
        locations
            .try_reserve_exact(sample.locations.len())
            .context("failed to reserve memory for sample locations")?;
        for location in &sample.locations {
            locations.push(self.try_add_location(location)?);
        }

        self.try_add_sample_internal(sample.values, labels, locations, timestamp)
    }

    /// Tries to add a sample using `api2` structures.
    pub fn try_add_sample2<'a, L: ExactSizeIterator<Item = anyhow::Result<api2::Label<'a>>>>(
        &mut self,
        locations: &[api2::Location2],
        values: &[i64],
        labels: L,
        timestamp: Option<Timestamp>,
    ) -> anyhow::Result<()> {
        let Some(translator) = &mut self.profiles_dictionary_translator else {
            anyhow::bail!("profiles dictionary not set");
        };

        // In debug builds, we iterate over the labels twice. That's not
        // something the trait bounds support, so we collect into a vector.
        // Since this is debug-only, the performance is fine.
        #[cfg(debug_assertions)]
        let labels = labels.collect::<Vec<_>>();
        #[cfg(debug_assertions)]
        {
            Self::validate_sample_labels2(labels.as_slice())?;
        }

        let string_table = &mut self.strings;
        let functions = &mut self.functions;
        let mappings = &mut self.mappings;
        let locations_set = &mut self.locations;
        let labels_set = &mut self.labels;

        let labels = {
            let mut lbls = Vec::new();
            lbls.try_reserve_exact(labels.len())?;
            for label in labels {
                let label = label.context("profile label failed to convert")?;
                let key = translator.translate_string(string_table, label.key)?;
                let internal_label = if !label.str.is_empty() {
                    let str = string_table.try_intern(label.str)?;
                    Label::str(key, str)
                } else {
                    let num = label.num;
                    let num_unit = string_table.try_intern(label.num_unit)?;
                    Label::num(key, num, num_unit)
                };

                let id = labels_set.try_dedup(internal_label)?;
                lbls.push(id);
            }
            lbls.into_boxed_slice()
        };

        let mut internal_locations = Vec::new();
        internal_locations
            .try_reserve_exact(locations.len())
            .context("failed to reserve memory for sample locations")?;
        for location in locations {
            let l = Location {
                mapping_id: translator.translate_mapping(
                    mappings,
                    string_table,
                    location.mapping,
                )?,
                function_id: translator.translate_function(
                    functions,
                    string_table,
                    location.function,
                )?,
                address: location.address,
                line: location.line,
            };
            let location_id = locations_set.checked_dedup(l)?;
            internal_locations.push(location_id);
        }

        self.try_add_sample_internal(values, labels, internal_locations, timestamp)
    }

    /// Gets the profiles dictionary, needed for `api2` operations.
    pub fn get_profiles_dictionary(&self) -> Option<&ProfilesDictionary> {
        self.profiles_dictionary_translator
            .as_ref()
            .map(|p| p.profiles_dictionary.deref())
    }

    pub fn add_string_id_sample(
        &mut self,
        sample: api::StringIdSample,
        timestamp: Option<Timestamp>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.string_storage.is_some(),
            "Current sample makes use of ManagedStringIds but profile was not created using a string table"
        );

        self.validate_string_id_sample_labels(&sample)?;

        let labels = sample
            .labels
            .iter()
            .map(|label| -> anyhow::Result<LabelId> {
                let key = self.resolve(label.key)?;
                let internal_label = if label.str != ManagedStringId::empty() {
                    let str = self.resolve(label.str)?;
                    Label::str(key, str)
                } else {
                    let num = label.num;
                    let num_unit = self.resolve(label.num_unit)?;
                    Label::num(key, num, num_unit)
                };

                self.labels.try_dedup(internal_label)
            })
            .collect::<Result<Box<[_]>, _>>()?;

        let mut locations = Vec::new();
        locations.try_reserve_exact(sample.locations.len())?;
        for location in &sample.locations {
            locations.push(self.add_string_id_location(location)?);
        }

        self.try_add_sample_internal(sample.values, labels, locations, timestamp)
    }

    fn try_add_sample_internal(
        &mut self,
        values: &[i64],
        labels: Box<[LabelId]>,
        locations: Vec<LocationId>,
        timestamp: Option<Timestamp>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            values.len() == self.sample_types.len(),
            "expected {} sample types, but sample had {} sample types",
            self.sample_types.len(),
            values.len(),
        );

        let labels = self.label_sets.try_dedup(LabelSet::new(labels))?;

        let stacktrace = self.try_add_stacktrace(locations)?;
        self.observations
            .add(Sample::new(labels, stacktrace), timestamp, values)?;
        Ok(())
    }

    pub fn add_upscaling_rule(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        upscaling_info: UpscalingInfo,
    ) -> anyhow::Result<()> {
        let label_name_id = self.try_intern(label_name)?;
        let label_value_id = self.try_intern(label_value)?;
        self.upscaling_rules.add(
            offset_values,
            (label_name, label_name_id),
            (label_value, label_value_id),
            upscaling_info,
            self.sample_types.len(),
        )?;

        Ok(())
    }

    pub fn get_generation(&self) -> anyhow::Result<Generation> {
        Ok(self.generation)
    }

    pub fn resolve(&mut self, id: ManagedStringId) -> anyhow::Result<StringId> {
        let non_empty_string_id = if let Some(valid_id) = NonZeroU32::new(id.value) {
            valid_id
        } else {
            return Ok(StringId::ZERO); // Both string tables use zero for the empty string
        };

        let string_storage = self.string_storage
            .as_ref()
            // Safety: We always get here through a direct or indirect call to add_string_id_sample,
            // which already ensured that the string storage exists.
            .ok_or_else(|| anyhow::anyhow!("Current sample makes use of ManagedStringIds but profile was not created using a string table"))?;

        let mut write_locked_storage = string_storage
            .lock()
            .map_err(|_| anyhow::anyhow!("string storage lock was poisoned"))?;

        let cached_profile_id = match self.string_storage_cached_profile_id.as_ref() {
            Some(cached_profile_id) => cached_profile_id,
            None => {
                let new_id = write_locked_storage.next_cached_profile_id()?;
                self.string_storage_cached_profile_id.get_or_insert(new_id)
            }
        };

        write_locked_storage.get_seq_num(non_empty_string_id, &mut self.strings, cached_profile_id)
    }

    /// This is used heavily enough in tests to make a helper.
    #[cfg(test)]
    pub fn new(sample_types: &[api::ValueType], period: Option<api::Period>) -> Self {
        #[allow(clippy::unwrap_used)]
        Self::try_new(sample_types, period).unwrap()
    }

    /// Tries to create a profile with the given `period`.
    /// Initializes the string table to hold common strings such as:
    ///  - "" (the empty string)
    ///  - "local root span id"
    ///  - "trace endpoint"
    /// - "end_timestamp_ns"
    ///
    /// All other fields are default.
    ///
    /// It's recommended to use [`Profile::try_new2`] instead.
    pub fn try_new(
        sample_types: &[api::ValueType],
        period: Option<api::Period>,
    ) -> io::Result<Self> {
        Self::try_new_internal(
            Self::backup_period(period),
            Self::backup_sample_types(sample_types),
            None,
            None,
        )
    }

    /// Tries to create a profile with the given period and sample types. The
    /// [`StringId2`]s should belong to the provided [`ProfilesDictionary`].
    #[inline(never)]
    #[cold]
    pub fn try_new2(
        profiles_dictionary: crate::profiles::collections::Arc<ProfilesDictionary>,
        sample_types: &[ValueType2],
        period: Option<Period2>,
    ) -> io::Result<Self> {
        let mut owned_sample_types = Vec::new();
        // Using try_reserve_exact because it will be converted to a Box<[]>,
        // so excess capacity would just make that conversion more expensive.
        owned_sample_types.try_reserve_exact(sample_types.len())?;
        owned_sample_types.extend(sample_types.iter().map(owned_types::ValueType::from));
        Self::try_new_internal(
            period.map(owned_types::Period::from),
            Some(owned_sample_types.into_boxed_slice()),
            None,
            Some(ProfilesDictionaryTranslator::new(profiles_dictionary)),
        )
    }

    #[inline]
    pub fn try_with_string_storage(
        sample_types: &[api::ValueType],
        period: Option<api::Period>,
        string_storage: Arc<Mutex<ManagedStringStorage>>,
    ) -> io::Result<Self> {
        Self::try_new_internal(
            Self::backup_period(period),
            Self::backup_sample_types(sample_types),
            Some(string_storage),
            None,
        )
    }

    /// Resets all data except the sample types and period.
    /// Returns the previous Profile on success.
    #[inline]
    pub fn reset_and_return_previous(&mut self) -> anyhow::Result<Profile> {
        let current_active_samples = self.sample_block()?;
        anyhow::ensure!(
            current_active_samples == 0,
            "Can't rotate the profile, there are still active samples. Drain them and try again."
        );

        let profiles_dictionary_translator = self
            .profiles_dictionary_translator
            .as_ref()
            .map(|t| -> Result<_, ArcOverflow> {
                let dict = t.profiles_dictionary.try_clone()?;
                Ok(ProfilesDictionaryTranslator::new(dict))
            })
            .transpose()?;

        let mut profile = Profile::try_new_internal(
            self.owned_period.take(),
            self.owned_sample_types.take(),
            self.string_storage.clone(),
            profiles_dictionary_translator,
        )
        .context("failed to initialize new profile")?;

        std::mem::swap(&mut *self, &mut profile);
        Ok(profile)
    }

    /// Serialize the aggregated profile, adding the end time and duration.
    /// # Arguments
    /// * `end_time` - Optional end time of the profile. Passing None will use the current time.
    /// * `duration` - Optional duration of the profile. Passing None will try to calculate the
    ///   duration based on the end time minus the start time, but under anomalous conditions this
    ///   may fail as system clocks can be adjusted. The programmer may also accidentally pass an
    ///   earlier time. The duration will be set to zero these cases.
    pub fn serialize_into_compressed_pprof(
        self,
        end_time: Option<SystemTime>,
        duration: Option<Duration>,
    ) -> anyhow::Result<EncodedProfile> {
        // On 2023-08-23, we analyzed the uploaded tarball size per language.
        // These tarballs include 1 or more profiles, but for most languages
        // using libdatadog (all?) there is only 1 profile, so this is a good
        // proxy for the compressed, final size of the profiles.
        // We found that for all languages using libdatadog, the average
        // tarball was at least 18 KiB. This initial size of 32KiB should
        // definitely outperform starting at zero for time consumed, allocator
        // pressure, and allocator fragmentation.
        const INITIAL_PPROF_BUFFER_SIZE: usize = 32 * 1024;

        // 2025-10-16: a profile larger than 10 MiB will be skipped, but a
        // higher limit is accepted for upload. A limit of 16 MiB allows us to
        // be a little bit decoupled from the exact limit so that if the
        // backend decides to accept larger pprofs, clients don't have to be
        // recompiled. But setting a much higher limit would be wasteful.
        const MAX_PROFILE_SIZE: usize = 16 * 1024 * 1024;

        let mut compressor = Compressor::<DefaultProfileCodec>::try_new(
            INITIAL_PPROF_BUFFER_SIZE,
            MAX_PROFILE_SIZE,
            Self::COMPRESSION_LEVEL,
        )
        .context("failed to create compressor")?;

        let mut encoded_profile = self.encode(&mut compressor, end_time, duration)?;
        encoded_profile.buffer = compressor.finish()?;
        Ok(encoded_profile)
    }

    /// Encodes the profile. Note that the buffer will be empty. The caller
    /// needs to flush/finish the writer, then fill/replace the buffer.
    fn encode<W: io::Write>(
        mut self,
        writer: &mut W,
        end_time: Option<SystemTime>,
        duration: Option<Duration>,
    ) -> anyhow::Result<EncodedProfile> {
        let end = end_time.unwrap_or_else(SystemTime::now);
        let start = self.start_time;
        let endpoints_stats = std::mem::take(&mut self.endpoints.stats);
        let duration_nanos = duration
            .unwrap_or_else(|| {
                end.duration_since(start).unwrap_or({
                    // Let's not throw away the whole profile just because the clocks were wrong.
                    // todo: log that the clock went backward (or programmer mistake).
                    Duration::ZERO
                })
            })
            .as_nanos()
            .min(i64::MAX as u128) as i64;

        let mut extended_label_sets: Vec<Vec<Label>> = Vec::with_capacity(self.label_sets.len());

        for label_set in std::mem::take(&mut self.label_sets) {
            let endpoint_label = self.get_endpoint_for_label_set(&label_set)?;
            // Leave one space for the timestamp if needed
            let mut labels = Vec::with_capacity(
                label_set.len() + 1 + if endpoint_label.is_some() { 1 } else { 0 },
            );
            for l in label_set.iter() {
                labels.push(*self.get_label(*l)?);
            }
            if let Some(endpoint_label) = endpoint_label {
                labels.push(endpoint_label);
            }
            extended_label_sets.push(labels);
        }

        let iter = std::mem::take(&mut self.observations).try_into_iter()?;
        for (sample, timestamp, mut values) in iter {
            let labels = &mut extended_label_sets[sample.labels.to_raw_id()];
            let location_ids: Vec<_> = self
                .get_stacktrace(sample.stacktrace)?
                .locations
                .iter()
                .map(Id::to_raw_id)
                .collect();
            self.check_location_ids_are_valid(&location_ids, self.locations.len())?;
            self.upscaling_rules.upscale_values(&mut values, labels)?;

            // Use the extra slot in the labels vector to store the timestamp without any reallocs.
            if let Some(ts) = timestamp {
                labels.push(Label::num(self.timestamp_key, ts.get(), StringId::ZERO))
            }
            let pprof_labels: Vec<_> = labels.iter().map(protobuf::Label::from).collect();
            if timestamp.is_some() {
                labels.pop();
            }

            let item = protobuf::Sample {
                location_ids: Record::from(location_ids.as_slice()),
                values: Record::from(values.as_slice()),
                // SAFETY: converting &[Label] to &[Field<Label,..>] which is
                // safe, because Field is repr(transparent).
                labels: unsafe {
                    &*(pprof_labels.as_slice() as *const [protobuf::Label]
                        as *const [Record<protobuf::Label, 3, NO_OPT_ZERO>])
                },
            };

            Record::<_, 2, NO_OPT_ZERO>::from(item).encode(writer)?;
        }

        // `Sample`s must be emitted before `SampleTypes` since we consume
        // fields as we convert (using `into_iter`).  This allows Rust to
        // release memory faster, reducing our peak RSS, but means that we
        // must process fields in dependency order, regardless of the numeric
        // field index in the `pprof` protobuf.
        // It is valid to emit protobuf fields out of order. See example in:
        // https://protobuf.dev/programming-guides/encoding/#optional
        //
        // In this case, we use `sample_types` during upscaling of `samples`,
        // so we must serialize `Sample` before `SampleType`.
        for sample_type in self.sample_types.iter() {
            Record::<_, 1, NO_OPT_ZERO>::from(*sample_type).encode(writer)?;
        }

        for (offset, item) in self.mappings.into_iter().enumerate() {
            let mapping = protobuf::Mapping {
                id: Record::from((offset + 1) as u64),
                memory_start: Record::from(item.memory_start),
                memory_limit: Record::from(item.memory_limit),
                file_offset: Record::from(item.file_offset),
                filename: Record::from(item.filename),
                build_id: Record::from(item.build_id),
            };
            Record::<_, 3, NO_OPT_ZERO>::from(mapping).encode(writer)?;
        }

        for (offset, item) in self.locations.into_iter().enumerate() {
            let location = protobuf::Location {
                id: Record::from((offset + 1) as u64),
                mapping_id: Record::from(item.mapping_id.map(MappingId::into_raw_id).unwrap_or(0)),
                address: Record::from(item.address),
                line: Record::from(protobuf::Line {
                    function_id: Record::from(item.function_id.into_raw_id()),
                    lineno: Record::from(item.line),
                }),
            };
            Record::<_, 4, NO_OPT_ZERO>::from(location).encode(writer)?;
        }

        for (offset, item) in self.functions.into_iter().enumerate() {
            let function = protobuf::Function {
                id: Record::from((offset + 1) as u64),
                name: Record::from(item.name),
                system_name: Record::from(item.system_name),
                filename: Record::from(item.filename),
            };
            Record::<_, 5, NO_OPT_ZERO>::from(function).encode(writer)?;
        }

        let mut lender = self.strings.into_lending_iter();
        while let Some(item) = lender.next() {
            Record::<_, 6, NO_OPT_ZERO>::from(item).encode(writer)?;
        }

        let time_nanos = self
            .start_time
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |duration| {
                duration.as_nanos().min(i64::MAX as u128) as i64
            });

        Record::<_, 9, OPT_ZERO>::from(time_nanos).encode(writer)?;
        Record::<_, 10, OPT_ZERO>::from(duration_nanos).encode(writer)?;

        if let Some((period, period_type)) = self.period {
            Record::<_, 11, OPT_ZERO>::from(period_type).encode(writer)?;
            Record::<_, 12, OPT_ZERO>::from(period).encode(writer)?;
        };

        Ok(EncodedProfile {
            start,
            end,
            buffer: Vec::new(),
            endpoints_stats,
        })
    }

    pub fn set_start_time(&mut self, start_time: SystemTime) -> anyhow::Result<()> {
        self.start_time = start_time;
        Ok(())
    }

    pub fn with_start_time(mut self, start_time: SystemTime) -> anyhow::Result<Self> {
        self.set_start_time(start_time)?;
        Ok(self)
    }

    /// In incident 35390 (JIRA PROF-11456) we observed invalid location_ids being present in
    /// emitted profiles. We're doing extra checks here so that if we see incorrect ids again,
    /// we are 100% sure they were not introduced prior to this stage.
    fn check_location_ids_are_valid(&self, location_ids: &[u64], len: usize) -> anyhow::Result<()> {
        let len: u64 = u64::try_from(len)?;
        for id in location_ids.iter() {
            let id = *id;
            // Location ids start from 1, that's why they're <= len instead of < len
            anyhow::ensure!(
                id > 0 && id <= len,
                "invalid location id found during serialization {:?}, len was {:?}",
                id,
                len
            )
        }
        Ok(())
    }
}

/// Private helper functions
impl Profile {
    fn try_add_function(&mut self, function: &api::Function) -> anyhow::Result<FunctionId> {
        let name = self.try_intern(function.name)?;
        let system_name = self.try_intern(function.system_name)?;
        let filename = self.try_intern(function.filename)?;

        self.functions.try_dedup(Function {
            name,
            system_name,
            filename,
        })
    }

    fn add_string_id_function(
        &mut self,
        function: &api::StringIdFunction,
    ) -> anyhow::Result<FunctionId> {
        let name = self.resolve(function.name)?;
        let system_name = self.resolve(function.system_name)?;
        let filename = self.resolve(function.filename)?;

        self.functions.try_dedup(Function {
            name,
            system_name,
            filename,
        })
    }

    fn try_add_location(&mut self, location: &api::Location) -> anyhow::Result<LocationId> {
        let mapping_id = self.try_add_mapping(&location.mapping)?;
        let function_id = self.try_add_function(&location.function)?;
        self.locations.checked_dedup(Location {
            mapping_id,
            function_id,
            address: location.address,
            line: location.line,
        })
    }

    fn add_string_id_location(
        &mut self,
        location: &api::StringIdLocation,
    ) -> anyhow::Result<LocationId> {
        let mapping_id = self.add_string_id_mapping(&location.mapping)?;
        let function_id = self.add_string_id_function(&location.function)?;
        self.locations.checked_dedup(Location {
            mapping_id,
            function_id,
            address: location.address,
            line: location.line,
        })
    }

    fn try_add_mapping(&mut self, mapping: &api::Mapping) -> anyhow::Result<Option<MappingId>> {
        #[inline]
        fn is_zero_mapping(mapping: &api::Mapping) -> bool {
            // - PHP, Python, and Ruby use a mapping only as required.
            // - .NET uses only the filename.
            // - The native profiler uses all fields.
            // We strike a balance for optimizing for the dynamic languages
            // and the others by mixing branches and branchless programming.
            let filename = mapping.filename.len();
            let build_id = mapping.build_id.len();
            if 0 != (filename | build_id) {
                return false;
            }

            let memory_start = mapping.memory_start;
            let memory_limit = mapping.memory_limit;
            let file_offset = mapping.file_offset;
            0 == (memory_start | memory_limit | file_offset)
        }

        if is_zero_mapping(mapping) {
            return Ok(None);
        }

        let filename = self.try_intern(mapping.filename)?;
        let build_id = self.try_intern(mapping.build_id)?;

        let id = self.mappings.try_dedup(Mapping {
            memory_start: mapping.memory_start,
            memory_limit: mapping.memory_limit,
            file_offset: mapping.file_offset,
            filename,
            build_id,
        })?;
        Ok(Some(id))
    }

    fn add_string_id_mapping(
        &mut self,
        mapping: &api::StringIdMapping,
    ) -> anyhow::Result<Option<MappingId>> {
        #[inline]
        fn is_zero_mapping(mapping: &api::StringIdMapping) -> bool {
            // See the other is_zero_mapping for more info, but only Ruby is
            // using this API at the moment, so we optimize for the whole
            // thing being a zero representation.
            let memory_start = mapping.memory_start;
            let memory_limit = mapping.memory_limit;
            let file_offset = mapping.file_offset;
            let strings = (mapping.filename.value | mapping.build_id.value) as u64;
            0 == (memory_start | memory_limit | file_offset | strings)
        }

        if is_zero_mapping(mapping) {
            return Ok(None);
        }

        let filename = self.resolve(mapping.filename)?;
        let build_id = self.resolve(mapping.build_id)?;

        let id = self.mappings.try_dedup(Mapping {
            memory_start: mapping.memory_start,
            memory_limit: mapping.memory_limit,
            file_offset: mapping.file_offset,
            filename,
            build_id,
        })?;
        Ok(Some(id))
    }

    fn try_add_stacktrace(&mut self, locations: Vec<LocationId>) -> anyhow::Result<StackTraceId> {
        self.stack_traces.try_dedup(StackTrace { locations })
    }

    #[inline]
    fn backup_period(src: Option<api::Period>) -> Option<owned_types::Period> {
        src.as_ref().map(owned_types::Period::from)
    }

    #[inline]
    fn backup_sample_types(src: &[api::ValueType]) -> Option<Box<[owned_types::ValueType]>> {
        Some(src.iter().map(owned_types::ValueType::from).collect())
    }

    /// Fetches the endpoint information for the label. There may be errors,
    /// but there may also be no endpoint information for a given endpoint.
    /// Hence, the return type of Result<Option<_>, _>.
    fn get_endpoint_for_label(&self, label: &Label) -> anyhow::Result<Option<Label>> {
        anyhow::ensure!(
            label.get_key() == self.endpoints.local_root_span_id_label,
            "bug: get_endpoint_for_label should only be called on labels with the key \"local root span id\""
        );

        anyhow::ensure!(
            label.has_num_value(),
            "the local root span id label value must be sent as a number, not a string, given {:?}",
            label
        );

        let local_root_span_id = if let LabelValue::Num { num, .. } = label.get_value() {
            // Safety: the value is an u64, but pprof only has signed values, so we
            // transmute it; the backend does the same.
            #[allow(
                unknown_lints,
                unnecessary_transmutes,
                reason = "i64::cast_unsigned requires MSRV 1.87.0"
            )]
            unsafe {
                std::mem::transmute::<i64, u64>(*num)
            }
        } else {
            return Err(anyhow::format_err!("the local root span id label value must be sent as a number, not a string, given {:?}",
            label));
        };

        Ok(self
            .endpoints
            .mappings
            .get(&local_root_span_id)
            .map(|v| Label::str(self.endpoints.endpoint_label, *v)))
    }

    fn get_endpoint_for_label_set(&self, label_set: &LabelSet) -> anyhow::Result<Option<Label>> {
        if let Some(label) = label_set.iter().find_map(|id| {
            if let Ok(label) = self.get_label(*id) {
                if label.get_key() == self.endpoints.local_root_span_id_label {
                    return Some(label);
                }
            }
            None
        }) {
            self.get_endpoint_for_label(label)
        } else {
            Ok(None)
        }
    }

    fn get_label(&self, id: LabelId) -> anyhow::Result<&Label> {
        self.labels
            .get_index(id.to_offset())
            .context("LabelId to have a valid interned index")
    }

    #[allow(dead_code)]
    fn get_label_set(&self, id: LabelSetId) -> anyhow::Result<&LabelSet> {
        self.label_sets
            .get_index(id.to_offset())
            .context("LabelSetId to have a valid interned index")
    }

    fn get_stacktrace(&self, st: StackTraceId) -> anyhow::Result<&StackTrace> {
        self.stack_traces
            .get_index(st.to_raw_id())
            .with_context(|| format!("StackTraceId {st:?} to exist in profile"))
    }

    /// Interns the `str` as a string, returning the id in the string table.
    /// The empty string is guaranteed to have an id of [StringId::ZERO].
    #[inline]
    fn intern(&mut self, item: &str) -> StringId {
        self.strings.intern(item)
    }

    /// Interns the `str` as a string, returning the id in the string table.
    /// The empty string is guaranteed to have an id of [StringId::ZERO].
    #[inline]
    fn try_intern(&mut self, item: &str) -> Result<StringId, string_table::Error> {
        self.strings.try_intern(item)
    }

    /// Creates a profile from the period, sample types, and start time using
    /// the owned values.
    fn try_new_internal(
        owned_period: Option<owned_types::Period>,
        owned_sample_types: Option<Box<[owned_types::ValueType]>>,
        string_storage: Option<Arc<Mutex<ManagedStringStorage>>>,
        profiles_dictionary_translator: Option<ProfilesDictionaryTranslator>,
    ) -> io::Result<Self> {
        let start_time = SystemTime::now();
        let mut profile = Self {
            profiles_dictionary_translator,
            owned_period,
            owned_sample_types,
            active_samples: Default::default(),
            endpoints: Default::default(),
            functions: Default::default(),
            generation: Generation::new(),

            labels: Default::default(),
            label_sets: Default::default(),
            locations: Default::default(),
            mappings: Default::default(),
            observations: Default::default(),
            period: None,
            sample_types: Box::new([]),
            stack_traces: Default::default(),
            start_time,
            strings: Default::default(),
            string_storage,
            string_storage_cached_profile_id: None, /* Never reuse an id! See comments on
                                                     * CachedProfileId for why. */
            timestamp_key: Default::default(),
            upscaling_rules: Default::default(),
        };

        let _id = profile.intern("");
        debug_assert!(_id == StringId::ZERO);

        profile.endpoints.local_root_span_id_label = profile.intern("local root span id");
        profile.endpoints.endpoint_label = profile.intern("trace endpoint");
        profile.timestamp_key = profile.intern("end_timestamp_ns");

        // Break "cannot borrow `*self` as mutable because it is also borrowed
        // as immutable" by moving it out, borrowing it, and putting it back.
        let owned_sample_types = profile.owned_sample_types.take();
        profile.sample_types = match &owned_sample_types {
            None => Box::new([]),
            Some(sample_types) => sample_types
                .iter()
                .map(|sample_type| ValueType {
                    r#type: Record::from(profile.intern(&sample_type.typ)),
                    unit: Record::from(profile.intern(&sample_type.unit)),
                })
                .collect(),
        };
        profile.owned_sample_types = owned_sample_types;

        // Break "cannot borrow `*self` as mutable because it is also borrowed
        // as immutable" by moving it out, borrowing it, and putting it back.
        let owned_period = profile.owned_period.take();
        if let Some(owned_types::Period { value, typ }) = &owned_period {
            profile.period = Some((
                *value,
                ValueType {
                    r#type: Record::from(profile.intern(&typ.typ)),
                    unit: Record::from(profile.intern(&typ.unit)),
                },
            ));
        };
        profile.owned_period = owned_period;

        profile.observations = Observations::try_new(profile.sample_types.len())?;
        Ok(profile)
    }

    #[cfg(debug_assertions)]
    fn validate_sample_labels(&mut self, sample: &api::Sample) -> anyhow::Result<()> {
        let mut seen: HashMap<&str, &api::Label> = HashMap::new();

        for label in sample.labels.iter() {
            if let Some(duplicate) = seen.insert(label.key, label) {
                anyhow::bail!("Duplicate label on sample: {duplicate:?} {label:?}");
            }

            if label.key == "local root span id" {
                anyhow::ensure!(
                    label.str.is_empty() && label.num != 0,
                    "Invalid \"local root span id\" label: {label:?}"
                );
            }

            anyhow::ensure!(
                label.key != "end_timestamp_ns",
                "Timestamp should not be passed as a label {label:?}"
            );
        }
        Ok(())
    }

    fn validate_string_id_sample_labels(
        &mut self,
        sample: &api::StringIdSample,
    ) -> anyhow::Result<()> {
        let mut seen: HashMap<ManagedStringId, &api::StringIdLabel> = HashMap::new();

        for label in sample.labels.iter() {
            if let Some(duplicate) = seen.insert(label.key, label) {
                anyhow::bail!("Duplicate label on sample: {:?} {:?}", duplicate, label);
            }

            let key_id: StringId = self.resolve(label.key)?;

            if key_id == self.endpoints.local_root_span_id_label {
                anyhow::ensure!(
                    label.str != ManagedStringId::empty() && label.num != 0,
                    "Invalid \"local root span id\" label: {:?}",
                    label
                );
            }

            anyhow::ensure!(
                key_id != self.timestamp_key,
                "Timestamp should not be passed as a label {:?}",
                label
            );
        }
        Ok(())
    }

    #[cfg(debug_assertions)]
    fn validate_sample_labels2(labels: &[anyhow::Result<api2::Label>]) -> anyhow::Result<()> {
        use crate::profiles::collections::StringRef;
        let mut seen: HashMap<StringRef, &api2::Label> = HashMap::with_capacity(labels.len());

        for label in labels.iter() {
            let Ok(label) = label.as_ref() else {
                anyhow::bail!("profiling FFI label string failed to convert")
            };
            let key = StringRef::from(label.key);
            if let Some(duplicate) = seen.insert(key, label) {
                anyhow::bail!("Duplicate label on sample: {duplicate:?} {label:?}");
            }

            if key == StringRef::LOCAL_ROOT_SPAN_ID {
                anyhow::ensure!(
                    label.str.is_empty() && label.num != 0,
                    "Invalid \"local root span id\" label: {label:?}"
                );
            }

            anyhow::ensure!(
                key != StringRef::END_TIMESTAMP_NS,
                "Timestamp should not be passed as a label {label:?}"
            );
        }
        Ok(())
    }
}

/// For testing and debugging purposes
impl Profile {
    #[cfg(test)]
    fn interned_strings_count(&self) -> usize {
        self.strings.len()
    }

    // Ideally, these would be [cgf(test)]. But its used in other module's test
    // code, which would break if we did so. We could try to do something with
    // a test "feature", but this naming scheme is sufficient for now.
    pub fn only_for_testing_num_aggregated_samples(&self) -> usize {
        self.observations.aggregated_samples_count()
    }

    pub fn only_for_testing_num_timestamped_samples(&self) -> usize {
        self.observations.timestamped_samples_count()
    }
}

#[cfg(test)]
mod api_tests {
    use super::*;
    use crate::pprof::test_utils::{roundtrip_to_pprof, sorted_samples, string_table_fetch};
    use libdd_profiling_protobuf::prost_impls;
    use std::collections::HashSet;

    #[test]
    fn interning() {
        let sample_types = [api::ValueType::new("samples", "count")];
        let mut profiles = Profile::new(&sample_types, None);

        let expected_id = StringId::from_offset(profiles.interned_strings_count());

        let string = "a";
        let id1 = profiles.intern(string);
        let id2 = profiles.intern(string);

        assert_eq!(id1, id2);
        assert_eq!(id1, expected_id);
    }

    #[test]
    fn api() {
        let sample_types = [
            api::ValueType::new("samples", "count"),
            api::ValueType::new("wall-time", "nanoseconds"),
        ];

        let mapping = api::Mapping {
            filename: "php",
            ..Default::default()
        };

        let index = api::Function {
            filename: "index.php",
            ..Default::default()
        };

        let locations = vec![
            api::Location {
                mapping,
                function: api::Function {
                    name: "phpinfo",
                    system_name: "phpinfo",
                    filename: "index.php",
                },
                ..Default::default()
            },
            api::Location {
                mapping,
                function: index,
                line: 3,
                ..Default::default()
            },
        ];

        let mut profile = Profile::new(&sample_types, None);
        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 0);

        profile
            .try_add_sample(
                api::Sample {
                    locations,
                    values: &[1, 10000],
                    labels: vec![],
                },
                None,
            )
            .expect("add to succeed");

        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 1);
    }

    fn provide_distinct_locations() -> Profile {
        let sample_types = [api::ValueType::new("samples", "count")];

        let mapping = api::Mapping {
            filename: "php",
            ..Default::default()
        };

        let main_locations = vec![api::Location {
            mapping,
            function: api::Function {
                name: "{main}",
                system_name: "{main}",
                filename: "index.php",
            },
            ..Default::default()
        }];
        let test_locations = vec![api::Location {
            mapping,
            function: api::Function {
                name: "test",
                system_name: "test",
                filename: "index.php",
            },
            ..Default::default()
        }];
        let timestamp_locations = vec![api::Location {
            mapping,
            function: api::Function {
                name: "test",
                system_name: "test",
                filename: "index.php",
            },
            ..Default::default()
        }];

        let values = &[1];
        let labels = vec![api::Label {
            key: "pid",
            num: 101,
            ..Default::default()
        }];

        let main_sample = api::Sample {
            locations: main_locations,
            values,
            labels: labels.clone(),
        };

        let test_sample = api::Sample {
            locations: test_locations,
            values,
            labels: labels.clone(),
        };

        let timestamp_sample = api::Sample {
            locations: timestamp_locations,
            values,
            labels,
        };

        let mut profile = Profile::new(&sample_types, None);
        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 0);

        profile
            .try_add_sample(main_sample, None)
            .expect("profile to not be full");
        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 1);

        profile
            .try_add_sample(test_sample, None)
            .expect("profile to not be full");
        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 2);

        assert_eq!(profile.only_for_testing_num_timestamped_samples(), 0);
        profile
            .try_add_sample(timestamp_sample, Timestamp::new(42))
            .expect("profile to not be full");
        assert_eq!(profile.only_for_testing_num_timestamped_samples(), 1);
        profile
    }

    #[test]
    fn impl_from_profile_for_pprof_profile() {
        let locations = provide_distinct_locations();
        let profile = roundtrip_to_pprof(locations).unwrap();

        assert_eq!(profile.samples.len(), 3);
        assert_eq!(profile.mappings.len(), 1);
        assert_eq!(profile.locations.len(), 2); // one of them dedups
        assert_eq!(profile.functions.len(), 2);

        for (index, mapping) in profile.mappings.iter().enumerate() {
            assert_eq!(
                (index + 1) as u64,
                mapping.id,
                "id {id} didn't match offset {offset} for {mapping:#?}",
                id = mapping.id,
                offset = index + 1
            );
        }

        for (index, location) in profile.locations.iter().enumerate() {
            assert_eq!((index + 1) as u64, location.id);
        }

        for (index, function) in profile.functions.iter().enumerate() {
            assert_eq!((index + 1) as u64, function.id);
        }
        let samples = sorted_samples(&profile);

        let sample = samples.first().expect("index 0 to exist");
        assert_eq!(sample.labels.len(), 1);
        let label = sample.labels.first().expect("index 0 to exist");
        let actual = api::Label {
            key: string_table_fetch(&profile, label.key),
            str: string_table_fetch(&profile, label.str),
            num: label.num,
            num_unit: string_table_fetch(&profile, label.num_unit),
        };
        let expected = api::Label {
            key: "pid",
            str: "",
            num: 101,
            num_unit: "",
        };
        assert_eq!(expected, actual);

        let sample = samples.get(2).expect("index 2 to exist");
        assert_eq!(sample.labels.len(), 2);
        let label = sample.labels.first().expect("index 0 to exist");
        let actual = api::Label {
            key: string_table_fetch(&profile, label.key),
            str: string_table_fetch(&profile, label.str),
            num: label.num,
            num_unit: string_table_fetch(&profile, label.num_unit),
        };
        let expected = api::Label {
            key: "pid",
            str: "",
            num: 101,
            num_unit: "",
        };
        assert_eq!(expected, actual);

        let label = sample.labels.get(1).expect("index 1 to exist");
        let actual = api::Label {
            key: string_table_fetch(&profile, label.key),
            str: string_table_fetch(&profile, label.str),
            num: label.num,
            num_unit: string_table_fetch(&profile, label.num_unit),
        };
        let expected = api::Label {
            key: "end_timestamp_ns",
            str: "",
            num: 42,
            num_unit: "",
        };
        assert_eq!(expected, actual);
        let key = string_table_fetch(&profile, label.key);
        let str = string_table_fetch(&profile, label.str);
        let num_unit = string_table_fetch(&profile, label.num_unit);
        assert_eq!(key, "end_timestamp_ns");
        assert_eq!(label.num, 42);
        assert_eq!(str, "");
        assert_eq!(num_unit, "");
    }

    #[test]
    fn reset() {
        let mut profile = provide_distinct_locations();
        /* This set of asserts is to make sure it's a non-empty profile that we
         * are working with so that we can test that reset works.
         */
        assert!(!profile.functions.is_empty());
        assert!(!profile.labels.is_empty());
        assert!(!profile.label_sets.is_empty());
        assert!(!profile.locations.is_empty());
        assert!(!profile.mappings.is_empty());
        assert!(!profile.observations.is_empty());
        assert!(!profile.sample_types.as_ref().is_empty());
        assert!(profile.period.is_none());
        assert!(profile.endpoints.mappings.is_empty());
        assert!(profile.endpoints.stats.is_empty());

        let prev = profile
            .reset_and_return_previous()
            .expect("reset to succeed");

        // These should all be empty now
        assert!(profile.functions.is_empty());
        assert!(profile.labels.is_empty());
        assert!(profile.label_sets.is_empty());
        assert!(profile.locations.is_empty());
        assert!(profile.mappings.is_empty());
        assert!(profile.observations.is_empty());
        assert!(profile.endpoints.mappings.is_empty());
        assert!(profile.endpoints.stats.is_empty());
        assert!(profile.upscaling_rules.is_empty());

        assert_eq!(profile.period, prev.period);
        assert_eq!(profile.sample_types, prev.sample_types);

        // The string table should have at least the empty string.
        assert!(profile.strings.len() > 0);
    }

    #[test]
    fn reset_period() {
        /* The previous test (reset) checked quite a few properties already, so
         * this one will focus only on the period.
         */
        let sample_types = [api::ValueType::new("wall-time", "nanoseconds")];
        let period = api::Period {
            r#type: sample_types[0],
            value: 10_000_000,
        };
        let mut profile = Profile::new(&sample_types, Some(period));

        let prev = profile
            .reset_and_return_previous()
            .expect("reset to succeed");

        // Resolve the string values to check that they match (their string
        // table offsets may not match).
        let mut strings: Vec<Box<str>> = Vec::with_capacity(profile.strings.len());
        let mut strings_iter = profile.strings.into_lending_iter();
        while let Some(item) = strings_iter.next() {
            strings.push(Box::from(item));
        }

        for (value, period_type) in [profile.period.unwrap(), prev.period.unwrap()] {
            assert_eq!(value, period.value);
            let r#type: &str = &strings[usize::from(period_type.r#type.value)];
            let unit: &str = &strings[usize::from(period_type.unit.value)];
            assert_eq!(r#type, period.r#type.r#type);
            assert_eq!(unit, period.r#type.unit);
        }
    }

    #[test]
    fn adding_local_root_span_id_with_string_value_fails() {
        let sample_types = [api::ValueType::new("wall-time", "nanoseconds")];

        let mut profile: Profile = Profile::new(&sample_types, None);

        let id_label = api::Label {
            key: "local root span id",
            str: "10", // bad value, should use .num instead for local root span id
            num: 0,
            num_unit: "",
        };

        let sample = api::Sample {
            locations: vec![],
            values: &[1, 10000],
            labels: vec![id_label],
        };

        assert!(profile.try_add_sample(sample, None).is_err());
    }

    #[test]
    fn lazy_endpoints() -> anyhow::Result<()> {
        let sample_types = [
            api::ValueType::new("samples", "count"),
            api::ValueType::new("wall-time", "nanoseconds"),
        ];

        let mut profile: Profile = Profile::new(&sample_types, None);

        let id_label = api::Label {
            key: "local root span id",
            str: "",
            num: 10,
            num_unit: "",
        };

        let id2_label = api::Label {
            key: "local root span id",
            str: "",
            num: 11,
            num_unit: "",
        };

        let other_label = api::Label {
            key: "other",
            str: "test",
            num: 0,
            num_unit: "",
        };

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 10000],
            labels: vec![id_label, other_label],
        };

        let sample2 = api::Sample {
            locations: vec![],
            values: &[1, 10000],
            labels: vec![id2_label, other_label],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");

        profile
            .try_add_sample(sample2, None)
            .expect("add to success");

        profile.add_endpoint(10, Cow::from("my endpoint"))?;

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();
        assert_eq!(serialized_profile.samples.len(), 2);
        let samples = sorted_samples(&serialized_profile);

        let s1 = samples.first().expect("sample");

        // The trace endpoint label should be added to the first sample
        assert_eq!(s1.labels.len(), 3);

        let l1 = s1.labels.first().expect("label");

        assert_eq!(
            string_table_fetch(&serialized_profile, l1.key),
            "local root span id"
        );
        assert_eq!(l1.num, 10);

        let l2 = s1.labels.get(1).expect("label");

        assert_eq!(string_table_fetch(&serialized_profile, l2.key), "other");
        assert_eq!(string_table_fetch(&serialized_profile, l2.str), "test");

        let l3 = s1.labels.get(2).expect("label");

        assert_eq!(
            string_table_fetch(&serialized_profile, l3.key),
            "trace endpoint"
        );
        assert_eq!(
            string_table_fetch(&serialized_profile, l3.str),
            "my endpoint"
        );

        let s2 = samples.get(1).expect("sample");

        // The trace endpoint label shouldn't be added to second sample because the span id doesn't
        // match
        assert_eq!(s2.labels.len(), 2);
        Ok(())
    }

    #[test]
    fn endpoint_counts_empty_test() {
        let sample_types = [
            api::ValueType::new("samples", "count"),
            api::ValueType::new("wall-time", "nanoseconds"),
        ];

        let profile: Profile = Profile::new(&sample_types, None);

        let encoded_profile = profile
            .serialize_into_compressed_pprof(None, None)
            .expect("Unable to encode/serialize the profile");

        let endpoints_stats = encoded_profile.endpoints_stats;
        assert!(endpoints_stats.is_empty());
    }

    #[test]
    fn endpoint_counts_test() -> anyhow::Result<()> {
        let sample_types = [
            api::ValueType::new("samples", "count"),
            api::ValueType::new("wall-time", "nanoseconds"),
        ];

        let mut profile: Profile = Profile::new(&sample_types, None);

        let one_endpoint = "my endpoint";
        profile.add_endpoint_count(Cow::from(one_endpoint), 1)?;
        profile.add_endpoint_count(Cow::from(one_endpoint), 1)?;

        let second_endpoint = "other endpoint";
        profile.add_endpoint_count(Cow::from(second_endpoint), 1)?;

        let encoded_profile = profile
            .serialize_into_compressed_pprof(None, None)
            .expect("Unable to encode/serialize the profile");

        let endpoints_stats = encoded_profile.endpoints_stats;

        let mut count: HashMap<String, i64> = HashMap::new();
        count.insert(one_endpoint.to_string(), 2);
        count.insert(second_endpoint.to_string(), 1);

        let expected_endpoints_stats = ProfiledEndpointsStats::from(count);

        assert_eq!(endpoints_stats, expected_endpoints_stats);
        Ok(())
    }

    #[test]
    fn local_root_span_id_label_cannot_occur_more_than_once() {
        let sample_types = [api::ValueType::new("wall-time", "nanoseconds")];

        let mut profile: Profile = Profile::new(&sample_types, None);

        let labels = vec![
            api::Label {
                key: "local root span id",
                str: "",
                num: 5738080760940355267_i64,
                num_unit: "",
            },
            api::Label {
                key: "local root span id",
                str: "",
                num: 8182855815056056749_i64,
                num_unit: "",
            },
        ];

        let sample = api::Sample {
            locations: vec![],
            values: &[10000],
            labels,
        };

        profile.try_add_sample(sample, None).unwrap_err();
    }

    #[test]
    fn test_no_upscaling_if_no_rules() {
        let sample_types = vec![
            api::ValueType::new("samples", "count"),
            api::ValueType::new("wall-time", "nanoseconds"),
        ];

        let mut profile: Profile = Profile::new(&sample_types, None);

        let id_label = api::Label {
            key: "my label",
            str: "coco",
            num: 0,
            num_unit: "",
        };

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 10000],
            labels: vec![id_label],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values[0], 1);
        assert_eq!(first.values[1], 10000);
    }

    fn create_samples_types() -> Vec<api::ValueType<'static>> {
        vec![
            api::ValueType::new("samples", "count"),
            api::ValueType::new("wall-time", "nanoseconds"),
            api::ValueType::new("cpu-time", "nanoseconds"),
        ]
    }

    fn create_label(key: &'static str, str: &'static str) -> api::Label<'static> {
        api::Label {
            key,
            str,
            num: 0,
            num_unit: "",
        }
    }

    #[test]
    fn test_upscaling_by_value_a_zero_value() {
        let sample_types = create_samples_types();

        let mut profile = Profile::new(&sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: &[0, 10000, 42],
            labels: vec![],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        let values_offset = vec![0];
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![0, 10000, 42]);
    }

    #[test]
    fn test_upscaling_by_value_on_one_value() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 10000, 42],
            labels: vec![],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.7 };
        let values_offset = vec![0];
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![3, 10000, 42]);
    }

    #[test]
    fn test_upscaling_by_value_on_one_value_with_poisson() {
        let sample_types = create_samples_types();

        let mut profile = Profile::new(&sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 16, 29],
            labels: vec![],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");

        let upscaling_info = UpscalingInfo::Poisson {
            sum_value_offset: 1,
            count_value_offset: 2,
            sampling_distance: 10,
        };
        let values_offset: Vec<usize> = vec![1];
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![1, 298, 29]);
    }

    #[test]
    fn test_upscaling_by_value_on_one_value_with_poisson_count() {
        let sample_types = create_samples_types();

        let mut profile = Profile::new(&sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 16, 29],
            labels: vec![],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");

        let upscaling_info = UpscalingInfo::PoissonNonSampleTypeCount {
            sum_value_offset: 1,
            count_value: 29,
            sampling_distance: 10,
        };
        let values_offset: Vec<usize> = vec![1];
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![1, 298, 29]);
    }

    #[test]
    fn test_upscaling_by_value_on_zero_value_with_poisson() {
        let sample_types = create_samples_types();

        let mut profile = Profile::new(&sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 16, 0],
            labels: vec![],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");

        let upscaling_info = UpscalingInfo::Poisson {
            sum_value_offset: 1,
            count_value_offset: 2,
            sampling_distance: 10,
        };
        let values_offset: Vec<usize> = vec![1];
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![1, 16, 0]);
    }

    #[test]
    fn test_cannot_add_a_rule_with_invalid_poisson_info() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 16, 0],
            labels: vec![],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");

        // invalid sampling_distance value
        let upscaling_info = UpscalingInfo::Poisson {
            sum_value_offset: 1,
            count_value_offset: 2,
            sampling_distance: 0,
        };

        let values_offset: Vec<usize> = vec![1];
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect_err("Cannot add a rule if sampling_distance is equal to 0");

        // x value is greater than the number of value types
        let upscaling_info2 = UpscalingInfo::Poisson {
            sum_value_offset: 42,
            count_value_offset: 2,
            sampling_distance: 10,
        };
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info2)
            .expect_err("Cannot add a rule if the offset x is invalid");

        // y value is greater than the number of value types
        let upscaling_info3 = UpscalingInfo::Poisson {
            sum_value_offset: 1,
            count_value_offset: 42,
            sampling_distance: 10,
        };
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info3)
            .expect_err("Cannot add a rule if the offset y is invalid");
    }

    #[test]
    fn test_upscaling_by_value_on_two_values() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 10000, 21],
            labels: vec![],
        };

        let mapping = api::Mapping {
            filename: "php",
            ..Default::default()
        };

        let main_locations = vec![api::Location {
            mapping,
            function: api::Function {
                name: "{main}",
                system_name: "{main}",
                filename: "index.php",
            },
            address: 0,
            line: 0,
        }];

        let sample2 = api::Sample {
            locations: main_locations,
            values: &[5, 24, 99],
            labels: vec![],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");
        profile
            .try_add_sample(sample2, None)
            .expect("add to success");

        // upscale the first value and the last one
        let values_offset: Vec<usize> = vec![0, 2];

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();
        let samples = sorted_samples(&serialized_profile);
        let first = samples.first().expect("first sample");

        assert_eq!(first.values, vec![2, 10000, 42]);

        let second = samples.get(1).expect("second sample");

        assert_eq!(second.values, vec![10, 24, 198]);
    }

    #[test]
    fn test_upscaling_by_value_on_two_value_with_two_rules() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 10000, 21],
            labels: vec![],
        };

        let mapping = api::Mapping {
            filename: "php",
            ..Default::default()
        };

        let main_locations = vec![api::Location {
            mapping,
            function: api::Function {
                name: "{main}",
                system_name: "{main}",
                filename: "index.php",
            },
            ..Default::default()
        }];

        let sample2 = api::Sample {
            locations: main_locations,
            values: &[5, 24, 99],
            labels: vec![],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");
        profile
            .try_add_sample(sample2, None)
            .expect("add to success");

        let mut values_offset: Vec<usize> = vec![0];

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        // add another byvaluerule on the 3rd offset
        values_offset.clear();
        values_offset.push(2);

        let upscaling_info2 = UpscalingInfo::Proportional { scale: 5.0 };

        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info2)
            .expect("Rule added");

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();
        let samples = sorted_samples(&serialized_profile);
        let first = samples.first().expect("first sample");

        assert_eq!(first.values, vec![2, 10000, 105]);

        let second = samples.get(1).expect("second sample");

        assert_eq!(second.values, vec![10, 24, 495]);
    }

    #[test]
    fn test_no_upscaling_by_label_if_no_match() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        let id_label = create_label("my_label", "coco");

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 10000, 42],
            labels: vec![id_label],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");

        let values_offset: Vec<usize> = vec![0];

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(
                values_offset.as_slice(),
                "my label",
                "foobar",
                upscaling_info,
            )
            .expect("Rule added");

        let upscaling_info2 = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(
                values_offset.as_slice(),
                "my other label",
                "coco",
                upscaling_info2,
            )
            .expect("Rule added");

        let upscaling_info3 = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(
                values_offset.as_slice(),
                "my other label",
                "foobar",
                upscaling_info3,
            )
            .expect("Rule added");

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![1, 10000, 42]);
    }

    #[test]
    fn test_upscaling_by_label_on_one_value() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        let id_label = create_label("my label", "coco");

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 10000, 42],
            labels: vec![id_label],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        let values_offset: Vec<usize> = vec![0];
        profile
            .add_upscaling_rule(
                values_offset.as_slice(),
                id_label.key,
                id_label.str,
                upscaling_info,
            )
            .expect("Rule added");

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![2, 10000, 42]);
    }

    #[test]
    fn test_upscaling_by_label_on_only_sample_out_of_two() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        let id_label = create_label("my label", "coco");

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 10000, 42],
            labels: vec![id_label],
        };

        let mapping = api::Mapping {
            filename: "php",
            ..Default::default()
        };

        let main_locations = vec![api::Location {
            mapping,
            function: api::Function {
                name: "{main}",
                system_name: "{main}",
                filename: "index.php",
            },
            ..Default::default()
        }];

        let sample2 = api::Sample {
            locations: main_locations,
            values: &[5, 24, 99],
            labels: vec![],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");
        profile
            .try_add_sample(sample2, None)
            .expect("add to success");

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        let values_offset: Vec<usize> = vec![0];
        profile
            .add_upscaling_rule(
                values_offset.as_slice(),
                id_label.key,
                id_label.str,
                upscaling_info,
            )
            .expect("Rule added");

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();
        let samples = sorted_samples(&serialized_profile);

        let first = samples.first().expect("one sample");

        assert_eq!(first.values, vec![2, 10000, 42]);

        let second = samples.get(1).expect("one sample");

        assert_eq!(second.values, vec![5, 24, 99]);
    }

    #[test]
    fn test_upscaling_by_label_with_two_different_rules_on_two_different_sample() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        let id_no_match_label = create_label("another label", "do not care");

        let id_label = create_label("my label", "coco");

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 10000, 42],
            labels: vec![id_label, id_no_match_label],
        };

        let mapping = api::Mapping {
            filename: "php",
            ..Default::default()
        };

        let main_locations = vec![api::Location {
            mapping,
            function: api::Function {
                name: "{main}",
                system_name: "{main}",
                filename: "index.php",
            },
            ..Default::default()
        }];

        let id_label2 = api::Label {
            key: "my other label",
            str: "foobar",
            num: 10,
            num_unit: "",
        };

        let sample2 = api::Sample {
            locations: main_locations,
            values: &[5, 24, 99],
            labels: vec![id_no_match_label, id_label2],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");
        profile
            .try_add_sample(sample2, None)
            .expect("add to success");

        // add rule for the first sample on the 1st value
        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        let mut values_offset: Vec<usize> = vec![0];
        profile
            .add_upscaling_rule(
                values_offset.as_slice(),
                id_label.key,
                id_label.str,
                upscaling_info,
            )
            .expect("Rule added");

        // add rule for the second sample on the 3rd value
        let upscaling_info2 = UpscalingInfo::Proportional { scale: 10.0 };
        values_offset.clear();
        values_offset.push(2);
        profile
            .add_upscaling_rule(
                values_offset.as_slice(),
                id_label2.key,
                id_label2.str,
                upscaling_info2,
            )
            .expect("Rule added");

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();
        let samples = sorted_samples(&serialized_profile);
        let first = samples.first().expect("one sample");

        assert_eq!(first.values, vec![2, 10000, 42]);

        let second = samples.get(1).expect("one sample");

        assert_eq!(second.values, vec![5, 24, 990]);
    }

    #[test]
    fn test_upscaling_by_label_on_two_values() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        let id_label = create_label("my label", "coco");

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 10000, 42],
            labels: vec![id_label],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");

        // upscale samples and wall-time values
        let values_offset: Vec<usize> = vec![0, 1];

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(
                values_offset.as_slice(),
                id_label.key,
                id_label.str,
                upscaling_info,
            )
            .expect("Rule added");

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![2, 20000, 42]);
    }
    #[test]
    fn test_upscaling_by_value_and_by_label_different_values() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        let id_label = create_label("my label", "coco");

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 10000, 42],
            labels: vec![id_label],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        let mut value_offsets: Vec<usize> = vec![0];
        profile
            .add_upscaling_rule(value_offsets.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        // a bylabel rule on the third offset
        let upscaling_info2 = UpscalingInfo::Proportional { scale: 5.0 };
        value_offsets.clear();
        value_offsets.push(2);
        profile
            .add_upscaling_rule(
                value_offsets.as_slice(),
                id_label.key,
                id_label.str,
                upscaling_info2,
            )
            .expect("Rule added");

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![2, 10000, 210]);
    }

    #[test]
    fn test_add_same_byvalue_rule_twice() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        // adding same offsets
        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        let mut value_offsets: Vec<usize> = vec![0, 2];
        profile
            .add_upscaling_rule(value_offsets.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let upscaling_info2 = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(value_offsets.as_slice(), "", "", upscaling_info2)
            .expect_err("Duplicated rules");

        // adding offsets with overlap on 2
        value_offsets.clear();
        value_offsets.push(2);
        value_offsets.push(1);
        let upscaling_info3 = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(value_offsets.as_slice(), "", "", upscaling_info3)
            .expect_err("Duplicated rules");

        // same offsets in different order
        value_offsets.clear();
        value_offsets.push(2);
        value_offsets.push(0);
        let upscaling_info4 = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(value_offsets.as_slice(), "", "", upscaling_info4)
            .expect_err("Duplicated rules");
    }

    #[test]
    fn test_add_two_bylabel_rules_with_overlap_on_values() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        // adding same offsets
        let mut value_offsets: Vec<usize> = vec![0, 2];
        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(value_offsets.as_slice(), "my label", "coco", upscaling_info)
            .expect("Rule added");
        let upscaling_info2 = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(
                value_offsets.as_slice(),
                "my label",
                "coco",
                upscaling_info2,
            )
            .expect_err("Duplicated rules");

        // adding offsets with overlap on 2
        value_offsets.clear();
        value_offsets.append(&mut vec![2, 1]);
        let upscaling_info3 = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(
                value_offsets.as_slice(),
                "my label",
                "coco",
                upscaling_info3,
            )
            .expect_err("Duplicated rules");

        // same offsets in different order
        value_offsets.clear();
        value_offsets.push(2);
        value_offsets.push(0);
        let upscaling_info4 = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(
                value_offsets.as_slice(),
                "my label",
                "coco",
                upscaling_info4,
            )
            .expect_err("Duplicated rules");
    }

    #[test]
    fn test_fail_if_bylabel_rule_and_by_value_rule_with_overlap_on_values() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        // adding same offsets
        let mut value_offsets: Vec<usize> = vec![0, 2];
        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };

        // add by value rule
        profile
            .add_upscaling_rule(value_offsets.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        // add by-label rule
        let upscaling_info2 = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(
                value_offsets.as_slice(),
                "my label",
                "coco",
                upscaling_info2,
            )
            .expect_err("Duplicated rules");

        // adding offsets with overlap on 2
        value_offsets.clear();
        value_offsets.append(&mut vec![2, 1]);
        let upscaling_info3 = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(
                value_offsets.as_slice(),
                "my label",
                "coco",
                upscaling_info3,
            )
            .expect_err("Duplicated rules");

        // same offsets in different order
        value_offsets.clear();
        value_offsets.push(2);
        value_offsets.push(0);
        let upscaling_info4 = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(
                value_offsets.as_slice(),
                "my label",
                "coco",
                upscaling_info4,
            )
            .expect_err("Duplicated rules");
    }

    #[test]
    fn test_add_rule_with_offset_out_of_bound() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        // adding same offsets
        let by_value_offsets: Vec<usize> = vec![0, 4];
        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(
                by_value_offsets.as_slice(),
                "my label",
                "coco",
                upscaling_info,
            )
            .expect_err("Invalid offset");
    }

    #[test]
    fn test_add_rule_with_offset_out_of_bound_poisson_function() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        // adding same offsets
        let by_value_offsets: Vec<usize> = vec![0, 4];
        let upscaling_info = UpscalingInfo::Poisson {
            sum_value_offset: 1,
            count_value_offset: 100,
            sampling_distance: 1,
        };
        profile
            .add_upscaling_rule(
                by_value_offsets.as_slice(),
                "my label",
                "coco",
                upscaling_info,
            )
            .expect_err("Invalid offset");
    }

    #[test]
    fn test_add_rule_with_offset_out_of_bound_poisson_function2() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        // adding same offsets
        let by_value_offsets: Vec<usize> = vec![0, 4];
        let upscaling_info = UpscalingInfo::Poisson {
            sum_value_offset: 100,
            count_value_offset: 1,
            sampling_distance: 1,
        };
        profile
            .add_upscaling_rule(
                by_value_offsets.as_slice(),
                "my label",
                "coco",
                upscaling_info,
            )
            .expect_err("Invalid offset");
    }

    #[test]
    fn test_add_rule_with_offset_out_of_bound_poisson_function3() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        // adding same offsets
        let by_value_offsets: Vec<usize> = vec![0, 4];
        let upscaling_info = UpscalingInfo::Poisson {
            sum_value_offset: 1100,
            count_value_offset: 100,
            sampling_distance: 1,
        };
        profile
            .add_upscaling_rule(
                by_value_offsets.as_slice(),
                "my label",
                "coco",
                upscaling_info,
            )
            .expect_err("Invalid offset");
    }

    #[test]
    fn test_fails_when_adding_byvalue_rule_colliding_on_offset_with_existing_bylabel_rule() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(&sample_types, None);

        let id_label = create_label("my label", "coco");

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 10000, 42],
            labels: vec![id_label],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");

        let mut value_offsets: Vec<usize> = vec![0, 1];
        // Add by-label rule first
        let upscaling_info2 = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(
                value_offsets.as_slice(),
                id_label.key,
                id_label.str,
                upscaling_info2,
            )
            .expect("Rule added");

        // add by-value rule
        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        value_offsets.clear();
        value_offsets.push(0);
        profile
            .add_upscaling_rule(value_offsets.as_slice(), "", "", upscaling_info)
            .expect_err("Rule added");
    }

    #[test]
    fn local_root_span_id_label_as_i64() -> anyhow::Result<()> {
        let sample_types = vec![
            api::ValueType {
                r#type: "samples",
                unit: "count",
            },
            api::ValueType {
                r#type: "wall-time",
                unit: "nanoseconds",
            },
        ];

        let mut profile = Profile::new(&sample_types, None);

        let id_label = api::Label {
            key: "local root span id",
            str: "",
            num: 10,
            num_unit: "",
        };

        let large_span_id = u64::MAX;
        // Safety: an u64 can fit into an i64, and we're testing that it's not mis-handled.
        #[allow(
            unknown_lints,
            unnecessary_transmutes,
            reason = "u64::cast_signed requires MSRV 1.87.0"
        )]
        let large_num: i64 = unsafe { std::mem::transmute(large_span_id) };

        let id2_label = api::Label {
            key: "local root span id",
            str: "",
            num: large_num,
            num_unit: "",
        };

        let sample1 = api::Sample {
            locations: vec![],
            values: &[1, 10000],
            labels: vec![id_label],
        };

        let sample2 = api::Sample {
            locations: vec![],
            values: &[1, 10000],
            labels: vec![id2_label],
        };

        profile
            .try_add_sample(sample1, None)
            .expect("add to success");
        profile
            .try_add_sample(sample2, None)
            .expect("add to success");

        profile.add_endpoint(10, Cow::from("endpoint 10"))?;
        profile.add_endpoint(large_span_id, Cow::from("large endpoint"))?;

        let serialized_profile = roundtrip_to_pprof(profile).unwrap();
        assert_eq!(serialized_profile.samples.len(), 2);

        // Find common label strings in the string table.
        let locate_string = |string: &str| -> i64 {
            // The table is supposed to be unique, so we shouldn't have to worry about duplicates.
            serialized_profile
                .string_table
                .iter()
                .enumerate()
                .find_map(|(offset, str)| {
                    if str == string {
                        Some(offset as i64)
                    } else {
                        None
                    }
                })
                .unwrap()
        };

        let local_root_span_id = locate_string("local root span id");
        let trace_endpoint = locate_string("trace endpoint");

        // Set up the expected labels per sample
        let expected_labels = [
            [
                prost_impls::Label {
                    key: local_root_span_id,
                    num: large_num,
                    ..Default::default()
                },
                prost_impls::Label {
                    key: trace_endpoint,
                    str: locate_string("large endpoint"),
                    ..Default::default()
                },
            ],
            [
                prost_impls::Label {
                    key: local_root_span_id,
                    num: 10,
                    ..Default::default()
                },
                prost_impls::Label {
                    key: trace_endpoint,
                    str: locate_string("endpoint 10"),
                    ..Default::default()
                },
            ],
        ];

        // Finally, match the labels.
        for (sample, labels) in sorted_samples(&serialized_profile)
            .iter()
            .zip(expected_labels.iter())
        {
            assert_eq!(sample.labels, labels);
        }
        Ok(())
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_try_new2_and_try_add_sample2() {
        struct Frame {
            file_name: &'static str,
            line_number: u32,
            function_name: &'static str,
        }

        // Create a ProfilesDictionary with realistic data from Ruby app
        let dict = crate::profiles::datatypes::ProfilesDictionary::try_new().unwrap();

        // Create sample types
        let samples_type = dict.try_insert_str2("samples").unwrap();
        let count_unit = dict.try_insert_str2("count").unwrap();
        let sample_types = vec![ValueType2 {
            type_id: samples_type,
            unit_id: count_unit,
        }];

        // Ruby stack trace (leaf-to-root order)
        // Taken from a Ruby app, everything here is source-available
        let frames = [
            Frame { file_name: "/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/appsec/instrumentation/gateway/middleware.rb", line_number: 18, function_name: "call" },
            Frame { file_name: "/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/appsec/instrumentation/gateway.rb", line_number: 37, function_name: "push" },
            Frame { file_name: "/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/appsec/instrumentation/gateway.rb", line_number: 41, function_name: "push" },
            Frame { file_name: "/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/appsec/contrib/rack/request_middleware.rb", line_number: 85, function_name: "catch" },
            Frame { file_name: "/usr/local/lib/libruby.so.3.3", line_number: 0, function_name: "catch" },
            Frame { file_name: "/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/appsec/contrib/rack/request_middleware.rb", line_number: 82, function_name: "call" },
            Frame { file_name: "/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/tracing/contrib/rack/middlewares.rb", line_number: 70, function_name: "call" },
            Frame { file_name: "/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/tracing/contrib/rack/trace_proxy_middleware.rb", line_number: 17, function_name: "call" },
            Frame { file_name: "/usr/local/bundle/gems/datadog-2.18.0/lib/datadog/tracing/contrib/rack/middlewares.rb", line_number: 474, function_name: "call" },
            Frame { file_name: "/usr/local/bundle/gems/railties-7.0.8.7/lib/rails/engine.rb", line_number: 530, function_name: "call" },
            Frame { file_name: "/usr/local/bundle/gems/puma-6.4.3/lib/puma/configuration.rb", line_number: 272, function_name: "call" },
            Frame { file_name: "/usr/local/bundle/gems/puma-6.4.3/lib/puma/request.rb", line_number: 100, function_name: "handle_request" },
            Frame { file_name: "/usr/local/bundle/gems/puma-6.4.3/lib/puma/thread_pool.rb", line_number: 378, function_name: "with_force_shutdown" },
            Frame { file_name: "/usr/local/bundle/gems/puma-6.4.3/lib/puma/request.rb", line_number: 99, function_name: "handle_request" },
            Frame { file_name: "/usr/local/bundle/gems/puma-6.4.3/lib/puma/server.rb", line_number: 464, function_name: "process_client" },
            Frame { file_name: "/usr/local/bundle/gems/puma-6.4.3/lib/puma/server.rb", line_number: 245, function_name: "run" },
            Frame { file_name: "/usr/local/bundle/gems/puma-6.4.3/lib/puma/thread_pool.rb", line_number: 155, function_name: "spawn_thread" },
            Frame { file_name: "/usr/local/bundle/gems/logging-2.4.0/lib/logging/diagnostic_context.rb", line_number: 474, function_name: "create_with_logging_context" },
        ];

        // Create a fake mapping to exercise the code path (Ruby doesn't currently use mappings)
        let fake_mapping_filename = dict.try_insert_str2("/usr/lib/ruby.so").unwrap();
        let fake_mapping = crate::profiles::datatypes::Mapping2 {
            memory_start: 0x7f0000000000,
            memory_limit: 0x7f0000100000,
            file_offset: 0,
            filename: fake_mapping_filename,
            build_id: crate::profiles::datatypes::StringId2::default(),
        };
        let mapping_id = dict.try_insert_mapping2(fake_mapping).unwrap();

        // Create locations from frames
        let mut locations = Vec::new();
        for frame in &frames {
            let function_name_id = dict.try_insert_str2(frame.function_name).unwrap();
            let filename_id = dict.try_insert_str2(frame.file_name).unwrap();
            let function = crate::profiles::datatypes::Function2 {
                name: function_name_id,
                system_name: crate::profiles::datatypes::StringId2::default(),
                file_name: filename_id,
            };
            let function_id = dict.try_insert_function2(function).unwrap();

            locations.push(api2::Location2 {
                mapping: mapping_id,
                function: function_id,
                address: 0,
                line: frame.line_number as i64,
            });
        }

        // Wrap in Arc
        let dict = crate::profiles::collections::Arc::try_new(dict).unwrap();

        // Create profile with dictionary
        let mut profile =
            Profile::try_new2(dict.try_clone().unwrap(), &sample_types, None).unwrap();

        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 0);

        let values = vec![1i64];

        // Add sample without labels
        let labels_iter = std::iter::empty::<anyhow::Result<api2::Label>>();
        profile
            .try_add_sample2(&locations, &values, labels_iter, None)
            .expect("add to succeed");

        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 1);

        // Add same sample again - should aggregate
        let labels_iter = std::iter::empty::<anyhow::Result<api2::Label>>();
        profile
            .try_add_sample2(&locations, &values, labels_iter, None)
            .expect("add to succeed");

        // Still 1 sample because it aggregated
        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 1);

        // Test with labels
        let label_key = dict.try_insert_str2("thread_id").unwrap();
        let label_value = "worker-1";

        let labels_iter = std::iter::once(Ok(api2::Label::str(label_key, label_value)));
        profile
            .try_add_sample2(&locations, &values, labels_iter, None)
            .expect("add with label to succeed");

        // Should be 2 samples now (different label set)
        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 2);

        // Test with numeric label
        let thread_id_key = dict.try_insert_str2("thread_id_num").unwrap();
        let labels_iter = std::iter::once(Ok(api2::Label::num(thread_id_key, 42, "")));
        profile
            .try_add_sample2(&locations, &values, labels_iter, None)
            .expect("add with numeric label to succeed");

        // Should be 3 samples now
        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 3);

        // Verify the profile roundtrips correctly through pprof serialization
        let pprof = roundtrip_to_pprof(profile).unwrap();
        assert_eq!(pprof.samples.len(), 3);

        // Verify we have the expected sample type
        assert_eq!(pprof.sample_types.len(), 1);
        let sample_type = &pprof.sample_types[0];
        let type_str = &pprof.string_table[sample_type.r#type as usize];
        let unit_str = &pprof.string_table[sample_type.unit as usize];
        assert_eq!(type_str, "samples");
        assert_eq!(unit_str, "count");

        // Verify the mapping is present and has the correct filename
        assert_eq!(pprof.mappings.len(), 1);
        let mapping = &pprof.mappings[0];
        let mapping_filename = &pprof.string_table[mapping.filename as usize];
        assert_eq!(mapping_filename, "/usr/lib/ruby.so");

        // Verify all 18 locations are present in each sample (same stack)
        assert_eq!(pprof.samples[0].location_ids.len(), 18);
        assert_eq!(pprof.samples[1].location_ids.len(), 18);
        assert_eq!(pprof.samples[2].location_ids.len(), 18);

        // Verify all filenames and function names from our frames are present
        let mut expected_files = std::collections::HashSet::new();
        let mut expected_functions = std::collections::HashSet::new();
        for frame in &frames {
            expected_files.insert(frame.file_name);
            expected_functions.insert(frame.function_name);
        }

        let string_table_set: std::collections::HashSet<&str> =
            pprof.string_table.iter().map(|s| s.as_str()).collect();

        assert!(
            expected_files.is_subset(&string_table_set),
            "Missing files from string table: {:?}",
            expected_files
                .difference(&string_table_set)
                .collect::<Vec<_>>()
        );

        assert!(
            expected_functions.is_subset(&string_table_set),
            "Missing functions from string table: {:?}",
            expected_functions
                .difference(&string_table_set)
                .collect::<Vec<_>>()
        );

        // Verify the label keys and values we added are present in string table
        let expected_label_strings = ["thread_id", "thread_id_num", "worker-1"]
            .into_iter()
            .collect::<HashSet<&str>>();
        let diff = expected_label_strings
            .difference(&string_table_set)
            .collect::<Vec<_>>();
        assert!(
            diff.is_empty(),
            "Missing label strings from string table: {:?}",
            diff
        );

        // Verify sample values
        // We have 3 samples: one with value 2 (aggregated), two with value 1
        // Samples may be reordered, so collect and sort the values
        let mut values: Vec<i64> = pprof.samples.iter().map(|s| s.values[0]).collect();
        values.sort_unstable();
        assert_eq!(values, vec![1, 1, 2]);
    }

    #[test]
    fn test_regression_managed_string_table_correctly_maps_ids() {
        let storage = Arc::new(Mutex::new(ManagedStringStorage::new()));
        let hello_id: u32;
        let world_id: u32;

        {
            let mut storage_guard = storage.lock().unwrap();
            hello_id = storage_guard.intern("hello").unwrap();
            world_id = storage_guard.intern("world").unwrap();
        }

        let sample_types = [api::ValueType::new("samples", "count")];
        let mut profile =
            Profile::try_with_string_storage(&sample_types, None, storage.clone()).unwrap();

        let location = api::StringIdLocation {
            function: api::StringIdFunction {
                name: api::ManagedStringId { value: hello_id },
                filename: api::ManagedStringId { value: world_id },
                ..Default::default()
            },
            ..Default::default()
        };

        let sample = api::StringIdSample {
            locations: vec![location],
            values: &[1],
            labels: vec![],
        };

        profile.add_string_id_sample(sample.clone(), None).unwrap();
        profile.add_string_id_sample(sample.clone(), None).unwrap();

        let pprof_first_profile =
            roundtrip_to_pprof(profile.reset_and_return_previous().unwrap()).unwrap();

        assert!(pprof_first_profile
            .string_table
            .iter()
            .any(|s| s == "hello"));
        assert!(pprof_first_profile
            .string_table
            .iter()
            .any(|s| s == "world"));

        // If the cache invalidation on the managed string table is working correctly, these strings
        // get correctly re-added to the profile's string table

        profile.add_string_id_sample(sample.clone(), None).unwrap();
        profile.add_string_id_sample(sample.clone(), None).unwrap();
        let pprof_second_profile = roundtrip_to_pprof(profile).unwrap();

        assert!(pprof_second_profile
            .string_table
            .iter()
            .any(|s| s == "hello"));
        assert!(pprof_second_profile
            .string_table
            .iter()
            .any(|s| s == "world"));
    }
}
