// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod fuzz_tests;

use self::api::UpscalingInfo;
use super::*;
use crate::api;
use crate::api::PersistentStringId;
use crate::collections::identifiable::*;
use crate::collections::string_storage::ManagedStringStorage;
use crate::collections::string_table::StringTable;
use crate::iter::{IntoLendingIterator, LendingIterator};
use crate::pprof::sliced_proto::*;
use crate::serializer::CompressedProtobufSerializer;
use anyhow::Context;
use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::RwLock;
use std::time::{Duration, SystemTime};

pub struct Profile {
    /// When profiles are reset, the sample-types need to be preserved. This
    /// maintains them in a way that does not depend on the string table. The
    /// Option part is this is taken from the old profile and moved to the new
    /// one.
    owned_sample_types: Option<Box<[owned_types::ValueType]>>,
    /// When profiles are reset, the period needs to be preserved. This
    /// stores it in a way that does not depend on the string table.
    owned_period: Option<owned_types::Period>,
    endpoints: Endpoints,
    functions: FxIndexSet<Function>,
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
    string_storage: Option<Rc<RwLock<ManagedStringStorage>>>,
    timestamp_key: StringId,
    upscaling_rules: UpscalingRules,
}

pub struct EncodedProfile {
    pub start: SystemTime,
    pub end: SystemTime,
    pub buffer: Vec<u8>,
    pub endpoints_stats: ProfiledEndpointsStats,
}

/// Public API
impl Profile {
    /// Add the endpoint data to the endpoint mappings.
    /// The `endpoint` string will be interned.
    pub fn add_endpoint(
        &mut self,
        local_root_span_id: u64,
        endpoint: Cow<str>,
    ) -> anyhow::Result<()> {
        let interned_endpoint = self.intern(endpoint.as_ref());

        self.endpoints
            .mappings
            .insert(local_root_span_id, interned_endpoint);
        Ok(())
    }

    pub fn add_endpoint_count(&mut self, endpoint: Cow<str>, value: i64) -> anyhow::Result<()> {
        self.endpoints
            .stats
            .add_endpoint_count(endpoint.into_owned(), value);
        Ok(())
    }

    pub fn add_sample(
        &mut self,
        sample: api::Sample,
        timestamp: Option<Timestamp>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            sample.values.len() == self.sample_types.len(),
            "expected {} sample types, but sample had {} sample types",
            self.sample_types.len(),
            sample.values.len(),
        );

        self.validate_sample_labels(&sample)?;
        let labels: Vec<_> = sample
            .labels
            .iter()
            .map(|label| {
                let key = self.intern(label.key);
                let internal_label = if let Some(s) = label.str {
                    let str = self.intern(s);
                    Label::str(key, str)
                } else {
                    let num = label.num;
                    let num_unit = label.num_unit.map(|s| self.intern(s));
                    Label::num(key, num, num_unit)
                };

                self.labels.dedup(internal_label)
            })
            .collect();
        let labels = self.label_sets.dedup(LabelSet::new(labels));

        let locations = sample
            .locations
            .iter()
            .map(|l| self.add_location(l))
            .collect();

        let stacktrace = self.add_stacktrace(locations);
        self.observations
            .add(Sample::new(labels, stacktrace), timestamp, sample.values)?;
        Ok(())
    }

    pub fn add_string_id_sample(
        &mut self,
        sample: api::StringIdSample,
        timestamp: Option<Timestamp>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            sample.values.len() == self.sample_types.len(),
            "expected {} sample types, but sample had {} sample types",
            self.sample_types.len(),
            sample.values.len(),
        );

        self.validate_string_id_sample_labels(&sample)?;
        let labels: Vec<_> = sample
            .labels
            .iter()
            .map(|label| {
                let key = self.resolve(label.key);
                let internal_label = if let Some(s) = label.str {
                    let str = self.resolve(s);
                    Label::str(key, str)
                } else {
                    let num = label.num;
                    let num_unit = label.num_unit.map(|s| self.resolve(s));
                    Label::num(key, num, num_unit)
                };

                self.labels.dedup(internal_label)
            })
            .collect();
        let labels = self.label_sets.dedup(LabelSet::new(labels));

        let locations = sample
            .locations
            .iter()
            .map(|l| self.add_string_id_location(l))
            .collect();

        let stacktrace = self.add_stacktrace(locations);
        self.observations
            .add(Sample::new(labels, stacktrace), timestamp, sample.values)?;
        Ok(())
    }

    pub fn add_upscaling_rule(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        upscaling_info: UpscalingInfo,
    ) -> anyhow::Result<()> {
        let label_name_id = self.intern(label_name);
        let label_value_id = self.intern(label_value);
        self.upscaling_rules.add(
            offset_values,
            (label_name, label_name_id),
            (label_value, label_value_id),
            upscaling_info,
            self.sample_types.len(),
        )?;

        Ok(())
    }

    pub fn resolve(&mut self, id: PersistentStringId) -> StringId {
        self.string_storage
            .as_ref()
            .expect("resolution from id requires managed string storage")
            .read()
            .expect("acquisition of read lock on string storage should succeed")
            .get_seq_num(id.value, &mut self.strings)
    }

    /// Creates a profile with `start_time`.
    /// Initializes the string table to hold:
    ///  - "" (the empty string)
    ///  - "local root span id"
    ///  - "trace endpoint"
    ///
    /// All other fields are default.
    #[inline]
    pub fn new(
        start_time: SystemTime,
        sample_types: &[api::ValueType],
        period: Option<api::Period>,
    ) -> Self {
        Self::new_internal(
            Self::backup_period(period),
            Self::backup_sample_types(sample_types),
            start_time,
            None,
        )
    }

    #[inline]
    pub fn with_string_storage(
        start_time: SystemTime,
        sample_types: &[api::ValueType],
        period: Option<api::Period>,
        string_storage: Rc<RwLock<ManagedStringStorage>>,
    ) -> Self {
        Self::new_internal(
            Self::backup_period(period),
            Self::backup_sample_types(sample_types),
            start_time,
            Some(string_storage),
        )
    }

    /// Resets all data except the sample types and period.
    /// Returns the previous Profile on success.
    #[inline]
    pub fn reset_and_return_previous(
        &mut self,
        start_time: Option<SystemTime>,
    ) -> anyhow::Result<Profile> {
        let mut profile = Profile::new_internal(
            self.owned_period.take(),
            self.owned_sample_types.take(),
            start_time.unwrap_or_else(SystemTime::now),
            self.string_storage.clone(),
        );

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
        mut self,
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
        let (period, period_type) = match self.period {
            Some(tuple) => (tuple.0, Some(tuple.1.into())),
            None => (0, None),
        };

        // On 2023-08-23, we analyzed the uploaded tarball size per language.
        // These tarballs include 1 or more profiles, but for most languages
        // using libdatadog (all?) there is only 1 profile, so this is a good
        // proxy for the compressed, final size of the profiles.
        // We found that for all languages using libdatadog, the average
        // tarball was at least 18 KiB. Since these archives are compressed,
        // and because profiles compress well, especially ones with timeline
        // enabled (over 9x for some analyzed timeline profiles), this initial
        // size of 32KiB should definitely out-perform starting at zero for
        // time consumed, allocator pressure, and allocator fragmentation.
        const INITIAL_PPROF_BUFFER_SIZE: usize = 32 * 1024;
        let mut encoder = CompressedProtobufSerializer::with_capacity(INITIAL_PPROF_BUFFER_SIZE);

        for (sample, timestamp, mut values) in std::mem::take(&mut self.observations).into_iter() {
            let labels = self.enrich_sample_labels(sample, timestamp)?;
            let location_ids: Vec<_> = self
                .get_stacktrace(sample.stacktrace)?
                .locations
                .iter()
                .map(Id::to_raw_id)
                .collect();
            self.upscaling_rules.upscale_values(&mut values, &labels)?;

            let labels = labels.into_iter().map(pprof::Label::from).collect();
            let item = pprof::Sample {
                location_ids,
                values,
                labels,
            };

            encoder.encode(ProfileSamplesEntry::from(item))?;
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
            let item: pprof::ValueType = sample_type.into();
            encoder.encode(ProfileSampleTypesEntry::from(item))?;
        }

        for item in into_pprof_iter(self.mappings) {
            encoder.encode(ProfileMappingsEntry::from(item))?;
        }

        for item in into_pprof_iter(self.locations) {
            encoder.encode(ProfileLocationsEntry::from(item))?;
        }

        for item in into_pprof_iter(self.functions) {
            encoder.encode(ProfileFunctionsEntry::from(item))?;
        }

        let mut lender = self.strings.into_lending_iter();
        while let Some(item) = lender.next() {
            encoder.encode_string_table_entry(item)?;
        }

        encoder.encode(ProfileSimpler {
            time_nanos: self
                .start_time
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |duration| {
                    duration.as_nanos().min(i64::MAX as u128) as i64
                }),
            duration_nanos,
            period_type,
            period,
        })?;

        Ok(EncodedProfile {
            start,
            end,
            buffer: encoder.finish()?,
            endpoints_stats,
        })
    }
}

/// Private helper functions
impl Profile {
    fn add_function(&mut self, function: &api::Function) -> FunctionId {
        let name = self.intern(function.name);
        let system_name = self.intern(function.system_name);
        let filename = self.intern(function.filename);

        let start_line = function.start_line;
        self.functions.dedup(Function {
            name,
            system_name,
            filename,
            start_line,
        })
    }

    fn add_string_id_function(&mut self, function: &api::StringIdFunction) -> FunctionId {
        let name = self.resolve(function.name);
        let system_name = self.resolve(function.system_name);
        let filename = self.resolve(function.filename);

        let start_line = function.start_line;
        self.functions.dedup(Function {
            name,
            system_name,
            filename,
            start_line,
        })
    }

    fn add_location(&mut self, location: &api::Location) -> LocationId {
        let mapping_id = self.add_mapping(&location.mapping);
        let function_id = self.add_function(&location.function);
        self.locations.dedup(Location {
            mapping_id,
            function_id,
            address: location.address,
            line: location.line,
        })
    }

    fn add_string_id_location(&mut self, location: &api::StringIdLocation) -> LocationId {
        let mapping_id = self.add_string_id_mapping(&location.mapping);
        let function_id = self.add_string_id_function(&location.function);
        self.locations.dedup(Location {
            mapping_id,
            function_id,
            address: location.address,
            line: location.line,
        })
    }

    fn add_mapping(&mut self, mapping: &api::Mapping) -> MappingId {
        let filename = self.intern(mapping.filename);
        let build_id = self.intern(mapping.build_id);

        self.mappings.dedup(Mapping {
            memory_start: mapping.memory_start,
            memory_limit: mapping.memory_limit,
            file_offset: mapping.file_offset,
            filename,
            build_id,
        })
    }

    fn add_string_id_mapping(&mut self, mapping: &api::StringIdMapping) -> MappingId {
        let filename = self.resolve(mapping.filename);
        let build_id = self.resolve(mapping.build_id);

        self.mappings.dedup(Mapping {
            memory_start: mapping.memory_start,
            memory_limit: mapping.memory_limit,
            file_offset: mapping.file_offset,
            filename,
            build_id,
        })
    }

    fn add_stacktrace(&mut self, locations: Vec<LocationId>) -> StackTraceId {
        self.stack_traces.dedup(StackTrace { locations })
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
            // Safety: the value is a u64, but pprof only has signed values, so we
            // transmute it; the backend does the same.
            unsafe { std::intrinsics::transmute::<i64, u64>(*num) }
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

    fn get_endpoint_for_labels(&self, label_set_id: LabelSetId) -> anyhow::Result<Option<Label>> {
        let label = self.get_label_set(label_set_id)?.iter().find_map(|id| {
            if let Ok(label) = self.get_label(*id) {
                if label.get_key() == self.endpoints.local_root_span_id_label {
                    return Some(label);
                }
            }
            None
        });
        if let Some(label) = label {
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

    fn get_label_set(&self, id: LabelSetId) -> anyhow::Result<&LabelSet> {
        self.label_sets
            .get_index(id.to_offset())
            .context("LabelSetId to have a valid interned index")
    }

    fn get_stacktrace(&self, st: StackTraceId) -> anyhow::Result<&StackTrace> {
        self.stack_traces
            .get_index(st.to_raw_id())
            .with_context(|| format!("StackTraceId {:?} to exist in profile", st))
    }

    /// Interns the `str` as a string, returning the id in the string table.
    /// The empty string is guaranteed to have an id of [StringId::ZERO].
    #[inline]
    fn intern(&mut self, item: &str) -> StringId {
        self.strings.intern(item)
    }

    /// Creates a profile from the period, sample types, and start time using
    /// the owned values.
    #[inline(never)]
    fn new_internal(
        owned_period: Option<owned_types::Period>,
        owned_sample_types: Option<Box<[owned_types::ValueType]>>,
        start_time: SystemTime,
        string_storage: Option<Rc<RwLock<ManagedStringStorage>>>,
    ) -> Self {
        let mut profile = Self {
            owned_period,
            owned_sample_types,
            endpoints: Default::default(),
            functions: Default::default(),
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
                    r#type: profile.intern(&sample_type.typ),
                    unit: profile.intern(&sample_type.unit),
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
                    r#type: profile.intern(&typ.typ),
                    unit: profile.intern(&typ.unit),
                },
            ));
        };
        profile.owned_period = owned_period;

        profile.observations = Observations::new(profile.sample_types.len());
        profile
    }

    fn enrich_sample_labels(
        &self,
        sample: Sample,
        timestamp: Option<Timestamp>,
    ) -> anyhow::Result<Vec<Label>> {
        self.get_label_set(sample.labels)?
            .iter()
            .map(|l| self.get_label(*l).copied())
            .chain(self.get_endpoint_for_labels(sample.labels).transpose())
            .chain(timestamp.map(|ts| Ok(Label::num(self.timestamp_key, ts.get(), None))))
            .collect()
    }

    fn validate_sample_labels(&mut self, sample: &api::Sample) -> anyhow::Result<()> {
        let mut seen: HashMap<&str, &api::Label> = HashMap::new();

        for label in sample.labels.iter() {
            if let Some(duplicate) = seen.insert(label.key, label) {
                anyhow::bail!("Duplicate label on sample: {:?} {:?}", duplicate, label);
            }

            if label.key == "local root span id" {
                anyhow::ensure!(
                    label.str.is_none() && label.num != 0,
                    "Invalid \"local root span id\" label: {:?}",
                    label
                );
            }

            anyhow::ensure!(
                label.key != "end_timestamp_ns",
                "Timestamp should not be passed as a label {:?}",
                label
            );
        }
        Ok(())
    }

    fn validate_string_id_sample_labels(
        &mut self,
        sample: &api::StringIdSample,
    ) -> anyhow::Result<()> {
        let mut seen: HashMap<PersistentStringId, &api::StringIdLabel> = HashMap::new();

        for label in sample.labels.iter() {
            if let Some(duplicate) = seen.insert(label.key, label) {
                anyhow::bail!("Duplicate label on sample: {:?} {:?}", duplicate, label);
            }

            let key_id: StringId = self.resolve(label.key);

            if key_id == self.endpoints.local_root_span_id_label {
                anyhow::ensure!(
                    label.str.is_none() && label.num != 0,
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

    #[test]
    fn interning() {
        let sample_types = [api::ValueType::new("samples", "count")];
        let mut profiles = Profile::new(SystemTime::now(), &sample_types, None);

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
                    start_line: 0,
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

        let mut profile = Profile::new(SystemTime::now(), &sample_types, None);
        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 0);

        profile
            .add_sample(
                api::Sample {
                    locations,
                    values: vec![1, 10000],
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
                ..Default::default()
            },
            ..Default::default()
        }];
        let test_locations = vec![api::Location {
            mapping,
            function: api::Function {
                name: "test",
                system_name: "test",
                filename: "index.php",
                start_line: 3,
            },
            ..Default::default()
        }];
        let timestamp_locations = vec![api::Location {
            mapping,
            function: api::Function {
                name: "test",
                system_name: "test",
                filename: "index.php",
                start_line: 4,
            },
            ..Default::default()
        }];

        let values: Vec<i64> = vec![1];
        let labels = vec![api::Label {
            key: "pid",
            num: 101,
            ..Default::default()
        }];

        let main_sample = api::Sample {
            locations: main_locations,
            values: values.clone(),
            labels: labels.clone(),
        };

        let test_sample = api::Sample {
            locations: test_locations,
            values: values.clone(),
            labels: labels.clone(),
        };

        let timestamp_sample = api::Sample {
            locations: timestamp_locations,
            values,
            labels,
        };

        let mut profile = Profile::new(SystemTime::now(), &sample_types, None);
        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 0);

        profile
            .add_sample(main_sample, None)
            .expect("profile to not be full");
        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 1);

        profile
            .add_sample(test_sample, None)
            .expect("profile to not be full");
        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 2);

        assert_eq!(profile.only_for_testing_num_timestamped_samples(), 0);
        profile
            .add_sample(timestamp_sample, Timestamp::new(42))
            .expect("profile to not be full");
        assert_eq!(profile.only_for_testing_num_timestamped_samples(), 1);
        profile
    }

    #[test]
    fn impl_from_profile_for_pprof_profile() {
        let locations = provide_distinct_locations();
        let profile = pprof::roundtrip_to_pprof(locations).unwrap();

        assert_eq!(profile.samples.len(), 3);
        assert_eq!(profile.mappings.len(), 1);
        assert_eq!(profile.locations.len(), 3);
        assert_eq!(profile.functions.len(), 3);

        for (index, mapping) in profile.mappings.iter().enumerate() {
            assert_eq!((index + 1) as u64, mapping.id);
        }

        for (index, location) in profile.locations.iter().enumerate() {
            assert_eq!((index + 1) as u64, location.id);
        }

        for (index, function) in profile.functions.iter().enumerate() {
            assert_eq!((index + 1) as u64, function.id);
        }
        let samples = profile.sorted_samples();

        let sample = samples.first().expect("index 0 to exist");
        assert_eq!(sample.labels.len(), 1);
        let label = sample.labels.first().expect("index 0 to exist");
        let key = profile
            .string_table
            .get(label.key as usize)
            .expect("index to exist");
        let str = profile
            .string_table
            .get(label.str as usize)
            .expect("index to exist");
        let num_unit = profile
            .string_table
            .get(label.num_unit as usize)
            .expect("index to exist");
        assert_eq!(key, "pid");
        assert_eq!(label.num, 101);
        assert_eq!(str, "");
        assert_eq!(num_unit, "");

        let sample = samples.get(1).expect("index 1 to exist");
        assert_eq!(sample.labels.len(), 1);
        let label = sample.labels.first().expect("index 0 to exist");
        let key = profile
            .string_table
            .get(label.key as usize)
            .expect("index to exist");
        let str = profile
            .string_table
            .get(label.str as usize)
            .expect("index to exist");
        let num_unit = profile
            .string_table
            .get(label.num_unit as usize)
            .expect("index to exist");
        assert_eq!(key, "pid");
        assert_eq!(label.num, 101);
        assert_eq!(str, "");
        assert_eq!(num_unit, "");

        let sample = samples.get(2).expect("index 2 to exist");
        assert_eq!(sample.labels.len(), 2);
        let label = sample.labels.first().expect("index 0 to exist");
        let key = profile
            .string_table
            .get(label.key as usize)
            .expect("index to exist");
        let str = profile
            .string_table
            .get(label.str as usize)
            .expect("index to exist");
        let num_unit = profile
            .string_table
            .get(label.num_unit as usize)
            .expect("index to exist");
        assert_eq!(key, "pid");
        assert_eq!(label.num, 101);
        assert_eq!(str, "");
        assert_eq!(num_unit, "");
        let label = sample.labels.get(1).expect("index 1 to exist");
        let key = profile
            .string_table
            .get(label.key as usize)
            .expect("index to exist");
        let str = profile
            .string_table
            .get(label.str as usize)
            .expect("index to exist");
        let num_unit = profile
            .string_table
            .get(label.num_unit as usize)
            .expect("index to exist");
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
            .reset_and_return_previous(None)
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
        let mut profile = Profile::new(SystemTime::now(), &sample_types, Some(period));

        let prev = profile
            .reset_and_return_previous(None)
            .expect("reset to succeed");

        // Resolve the string values to check that they match (their string
        // table offsets may not match).
        let mut strings = Vec::with_capacity(profile.strings.len());
        let mut strings_iter = profile.strings.into_lending_iter();
        while let Some(item) = strings_iter.next() {
            strings.push(Box::from(String::from(item)));
        }

        for (value, period_type) in [profile.period.unwrap(), prev.period.unwrap()] {
            assert_eq!(value, period.value);
            let r#type: &str = &strings[period_type.r#type.to_offset()];
            let unit: &str = &strings[period_type.unit.to_offset()];
            assert_eq!(r#type, period.r#type.r#type);
            assert_eq!(unit, period.r#type.unit);
        }
    }

    #[test]
    fn adding_local_root_span_id_with_string_value_fails() {
        let sample_types = [api::ValueType::new("wall-time", "nanoseconds")];

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let id_label = api::Label {
            key: "local root span id",
            str: Some("10"), // bad value, should use .num instead for local root span id
            num: 0,
            num_unit: None,
        };

        let sample = api::Sample {
            locations: vec![],
            values: vec![1, 10000],
            labels: vec![id_label],
        };

        assert!(profile.add_sample(sample, None).is_err());
    }

    #[test]
    fn lazy_endpoints() -> anyhow::Result<()> {
        let sample_types = [
            api::ValueType::new("samples", "count"),
            api::ValueType::new("wall-time", "nanoseconds"),
        ];

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let id_label = api::Label {
            key: "local root span id",
            str: None,
            num: 10,
            num_unit: None,
        };

        let id2_label = api::Label {
            key: "local root span id",
            str: None,
            num: 11,
            num_unit: None,
        };

        let other_label = api::Label {
            key: "other",
            str: Some("test"),
            num: 0,
            num_unit: None,
        };

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000],
            labels: vec![id_label, other_label],
        };

        let sample2 = api::Sample {
            locations: vec![],
            values: vec![1, 10000],
            labels: vec![id2_label, other_label],
        };

        profile.add_sample(sample1, None).expect("add to success");

        profile.add_sample(sample2, None).expect("add to success");

        profile.add_endpoint(10, Cow::from("my endpoint"))?;

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();
        assert_eq!(serialized_profile.samples.len(), 2);
        let samples = serialized_profile.sorted_samples();

        let s1 = samples.first().expect("sample");

        // The trace endpoint label should be added to the first sample
        assert_eq!(s1.labels.len(), 3);

        let l1 = s1.labels.first().expect("label");

        assert_eq!(
            serialized_profile
                .string_table
                .get(l1.key as usize)
                .unwrap(),
            "local root span id"
        );
        assert_eq!(l1.num, 10);

        let l2 = s1.labels.get(1).expect("label");

        assert_eq!(
            serialized_profile
                .string_table
                .get(l2.key as usize)
                .unwrap(),
            "other"
        );
        assert_eq!(
            serialized_profile
                .string_table
                .get(l2.str as usize)
                .unwrap(),
            "test"
        );

        let l3 = s1.labels.get(2).expect("label");

        assert_eq!(
            serialized_profile
                .string_table
                .get(l3.key as usize)
                .unwrap(),
            "trace endpoint"
        );
        assert_eq!(
            serialized_profile
                .string_table
                .get(l3.str as usize)
                .unwrap(),
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

        let profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

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

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

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

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let labels = vec![
            api::Label {
                key: "local root span id",
                str: None,
                num: 5738080760940355267_i64,
                num_unit: None,
            },
            api::Label {
                key: "local root span id",
                str: None,
                num: 8182855815056056749_i64,
                num_unit: None,
            },
        ];

        let sample = api::Sample {
            locations: vec![],
            values: vec![10000],
            labels,
        };

        profile.add_sample(sample, None).unwrap_err();
    }

    #[test]
    fn test_no_upscaling_if_no_rules() {
        let sample_types = vec![
            api::ValueType::new("samples", "count"),
            api::ValueType::new("wall-time", "nanoseconds"),
        ];

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let id_label = api::Label {
            key: "my label",
            str: Some("coco"),
            num: 0,
            num_unit: None,
        };

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000],
            labels: vec![id_label],
        };

        profile.add_sample(sample1, None).expect("add to success");

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();

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

    fn create_label(key: &'static str, str: Option<&'static str>) -> api::Label<'static> {
        api::Label {
            key,
            str,
            num: 0,
            num_unit: None,
        }
    }

    #[test]
    fn test_upscaling_by_value_a_zero_value() {
        let sample_types = create_samples_types();

        let mut profile = Profile::new(SystemTime::now(), &sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![0, 10000, 42],
            labels: vec![],
        };

        profile.add_sample(sample1, None).expect("add to success");

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        let values_offset = vec![0];
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![0, 10000, 42]);
    }

    #[test]
    fn test_upscaling_by_value_on_one_value() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
            labels: vec![],
        };

        profile.add_sample(sample1, None).expect("add to success");

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.7 };
        let values_offset = vec![0];
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![3, 10000, 42]);
    }

    #[test]
    fn test_upscaling_by_value_on_one_value_with_poisson() {
        let sample_types = create_samples_types();

        let mut profile = Profile::new(SystemTime::now(), &sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 16, 29],
            labels: vec![],
        };

        profile.add_sample(sample1, None).expect("add to success");

        let upscaling_info = UpscalingInfo::Poisson {
            sum_value_offset: 1,
            count_value_offset: 2,
            sampling_distance: 10,
        };
        let values_offset: Vec<usize> = vec![1];
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![1, 298, 29]);
    }

    #[test]
    fn test_upscaling_by_value_on_zero_value_with_poisson() {
        let sample_types = create_samples_types();

        let mut profile = Profile::new(SystemTime::now(), &sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 16, 0],
            labels: vec![],
        };

        profile.add_sample(sample1, None).expect("add to success");

        let upscaling_info = UpscalingInfo::Poisson {
            sum_value_offset: 1,
            count_value_offset: 2,
            sampling_distance: 10,
        };
        let values_offset: Vec<usize> = vec![1];
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![1, 16, 0]);
    }

    #[test]
    fn test_cannot_add_a_rule_with_invalid_poisson_info() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 16, 0],
            labels: vec![],
        };

        profile.add_sample(sample1, None).expect("add to success");

        // invalid sampling_distance vaue
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

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 21],
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
                start_line: 0,
            },
            address: 0,
            line: 0,
        }];

        let sample2 = api::Sample {
            locations: main_locations,
            values: vec![5, 24, 99],
            labels: vec![],
        };

        profile.add_sample(sample1, None).expect("add to success");
        profile.add_sample(sample2, None).expect("add to success");

        // upscale the first value and the last one
        let values_offset: Vec<usize> = vec![0, 2];

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();
        let samples = serialized_profile.sorted_samples();
        let first = samples.first().expect("first sample");

        assert_eq!(first.values, vec![2, 10000, 42]);

        let second = samples.get(1).expect("second sample");

        assert_eq!(second.values, vec![10, 24, 198]);
    }

    #[test]
    fn test_upscaling_by_value_on_two_value_with_two_rules() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 21],
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
                start_line: 0,
            },
            ..Default::default()
        }];

        let sample2 = api::Sample {
            locations: main_locations,
            values: vec![5, 24, 99],
            labels: vec![],
        };

        profile.add_sample(sample1, None).expect("add to success");
        profile.add_sample(sample2, None).expect("add to success");

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

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();
        let samples = serialized_profile.sorted_samples();
        let first = samples.first().expect("first sample");

        assert_eq!(first.values, vec![2, 10000, 105]);

        let second = samples.get(1).expect("second sample");

        assert_eq!(second.values, vec![10, 24, 495]);
    }

    #[test]
    fn test_no_upscaling_by_label_if_no_match() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let id_label = create_label("my_label", Some("coco"));

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
            labels: vec![id_label],
        };

        profile.add_sample(sample1, None).expect("add to success");

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

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![1, 10000, 42]);
    }

    #[test]
    fn test_upscaling_by_label_on_one_value() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let id_label = create_label("my label", Some("coco"));

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
            labels: vec![id_label],
        };

        profile.add_sample(sample1, None).expect("add to success");

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        let values_offset: Vec<usize> = vec![0];
        profile
            .add_upscaling_rule(
                values_offset.as_slice(),
                id_label.key,
                id_label.str.unwrap(),
                upscaling_info,
            )
            .expect("Rule added");

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![2, 10000, 42]);
    }

    #[test]
    fn test_upscaling_by_label_on_only_sample_out_of_two() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let id_label = create_label("my label", Some("coco"));

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
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
                start_line: 0,
            },
            ..Default::default()
        }];

        let sample2 = api::Sample {
            locations: main_locations,
            values: vec![5, 24, 99],
            labels: vec![],
        };

        profile.add_sample(sample1, None).expect("add to success");
        profile.add_sample(sample2, None).expect("add to success");

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        let values_offset: Vec<usize> = vec![0];
        profile
            .add_upscaling_rule(
                values_offset.as_slice(),
                id_label.key,
                id_label.str.unwrap(),
                upscaling_info,
            )
            .expect("Rule added");

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();
        let samples = serialized_profile.sorted_samples();

        let first = samples.first().expect("one sample");

        assert_eq!(first.values, vec![2, 10000, 42]);

        let second = samples.get(1).expect("one sample");

        assert_eq!(second.values, vec![5, 24, 99]);
    }

    #[test]
    fn test_upscaling_by_label_with_two_different_rules_on_two_different_sample() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let id_no_match_label = create_label("another label", Some("do not care"));

        let id_label = create_label("my label", Some("coco"));

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
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
                start_line: 0,
            },
            ..Default::default()
        }];

        let id_label2 = api::Label {
            key: "my other label",
            str: Some("foobar"),
            num: 10,
            num_unit: None,
        };

        let sample2 = api::Sample {
            locations: main_locations,
            values: vec![5, 24, 99],
            labels: vec![id_no_match_label, id_label2],
        };

        profile.add_sample(sample1, None).expect("add to success");
        profile.add_sample(sample2, None).expect("add to success");

        // add rule for the first sample on the 1st value
        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        let mut values_offset: Vec<usize> = vec![0];
        profile
            .add_upscaling_rule(
                values_offset.as_slice(),
                id_label.key,
                id_label.str.unwrap(),
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
                id_label2.str.unwrap(),
                upscaling_info2,
            )
            .expect("Rule added");

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();
        let samples = serialized_profile.sorted_samples();
        let first = samples.first().expect("one sample");

        assert_eq!(first.values, vec![2, 10000, 42]);

        let second = samples.get(1).expect("one sample");

        assert_eq!(second.values, vec![5, 24, 990]);
    }

    #[test]
    fn test_upscaling_by_label_on_two_values() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let id_label = create_label("my label", Some("coco"));

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
            labels: vec![id_label],
        };

        profile.add_sample(sample1, None).expect("add to success");

        // upscale samples and wall-time values
        let values_offset: Vec<usize> = vec![0, 1];

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(
                values_offset.as_slice(),
                id_label.key,
                id_label.str.unwrap(),
                upscaling_info,
            )
            .expect("Rule added");

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![2, 20000, 42]);
    }
    #[test]
    fn test_upscaling_by_value_and_by_label_different_values() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let id_label = create_label("my label", Some("coco"));

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
            labels: vec![id_label],
        };

        profile.add_sample(sample1, None).expect("add to success");

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
                id_label.str.unwrap(),
                upscaling_info2,
            )
            .expect("Rule added");

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();

        assert_eq!(serialized_profile.samples.len(), 1);
        let first = serialized_profile.samples.first().expect("one sample");

        assert_eq!(first.values, vec![2, 10000, 210]);
    }

    #[test]
    fn test_add_same_byvalue_rule_twice() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

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

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

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

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

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

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

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

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

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

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

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

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

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
    fn test_fails_when_adding_byvalue_rule_collinding_on_offset_with_existing_bylabel_rule() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::new(SystemTime::now(), &sample_types, None);

        let id_label = create_label("my label", Some("coco"));

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
            labels: vec![id_label],
        };

        profile.add_sample(sample1, None).expect("add to success");

        let mut value_offsets: Vec<usize> = vec![0, 1];
        // Add by-label rule first
        let upscaling_info2 = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(
                value_offsets.as_slice(),
                id_label.key,
                id_label.str.unwrap(),
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

        let mut profile = Profile::new(SystemTime::now(), &sample_types, None);

        let id_label = api::Label {
            key: "local root span id",
            str: None,
            num: 10,
            num_unit: None,
        };

        let large_span_id = u64::MAX;
        // Safety: a u64 can fit into an i64, and we're testing that it's not mis-handled.
        let large_num: i64 = unsafe { std::intrinsics::transmute(large_span_id) };

        let id2_label = api::Label {
            key: "local root span id",
            str: None,
            num: large_num,
            num_unit: None,
        };

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000],
            labels: vec![id_label],
        };

        let sample2 = api::Sample {
            locations: vec![],
            values: vec![1, 10000],
            labels: vec![id2_label],
        };

        profile.add_sample(sample1, None).expect("add to success");
        profile.add_sample(sample2, None).expect("add to success");

        profile.add_endpoint(10, Cow::from("endpoint 10"))?;
        profile.add_endpoint(large_span_id, Cow::from("large endpoint"))?;

        let serialized_profile = pprof::roundtrip_to_pprof(profile).unwrap();
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
                pprof::Label {
                    key: local_root_span_id,
                    str: 0,
                    num: large_num,
                    num_unit: 0,
                },
                pprof::Label::str(trace_endpoint, locate_string("large endpoint")),
            ],
            [
                pprof::Label {
                    key: local_root_span_id,
                    str: 0,
                    num: 10,
                    num_unit: 0,
                },
                pprof::Label::str(trace_endpoint, locate_string("endpoint 10")),
            ],
        ];

        // Finally, match the labels.
        for (sample, labels) in serialized_profile
            .sorted_samples()
            .iter()
            .zip(expected_labels.iter())
        {
            assert_eq!(sample.labels, labels);
        }
        Ok(())
    }
}
