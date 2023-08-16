// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

pub mod api;
pub mod internal;
pub mod pprof;
pub mod profiled_endpoints;

use std::borrow::Cow;
use std::collections::HashMap;
use std::convert::TryInto;
use std::hash::{BuildHasherDefault, Hash};
use std::num::NonZeroI64;
use std::time::{Duration, SystemTime};

use internal::*;
use profiled_endpoints::ProfiledEndpointsStats;
use prost::{EncodeError, Message};

use self::api::UpscalingInfo;

pub type FxIndexMap<K, V> = indexmap::IndexMap<K, V, BuildHasherDefault<rustc_hash::FxHasher>>;
pub type FxIndexSet<K> = indexmap::IndexSet<K, BuildHasherDefault<rustc_hash::FxHasher>>;

pub type Timestamp = NonZeroI64;

#[derive(Eq, PartialEq, Hash)]
struct Sample {
    pub stacktrace: StackTraceId,

    /// label includes additional context for this sample. It can include
    /// things like a thread id, allocation size, etc
    pub labels: Vec<Label>,

    /// Offset into `labels` for the label with key == "local root span id".
    local_root_span_id_label_offset: Option<usize>,
}

pub struct UpscalingRule {
    values_offset: Vec<usize>,
    upscaling_info: UpscalingInfo,
}

impl UpscalingRule {
    pub fn compute_scale(&self, values: &[i64]) -> f64 {
        match self.upscaling_info {
            UpscalingInfo::Poisson {
                sum_value_offset,
                count_value_offset,
                sampling_distance,
            } => {
                // This should not happen, but if it happens,
                // do not upscale
                if values[sum_value_offset] == 0 || values[count_value_offset] == 0 {
                    return 1_f64;
                }

                let avg = values[sum_value_offset] as f64 / values[count_value_offset] as f64;
                1_f64 / (1_f64 - (-avg / sampling_distance as f64).exp())
            }
            UpscalingInfo::Proportional { scale } => scale,
        }
    }
}

pub struct Profile {
    aggregated_samples: HashMap<Sample, Vec<i64>>,
    endpoints: Endpoints,
    functions: FxIndexSet<Function>,
    locations: FxIndexSet<Location>,
    mappings: FxIndexSet<Mapping>,
    period: Option<(i64, ValueType)>,
    sample_types: Vec<ValueType>,
    stack_traces: FxIndexSet<StackTrace>,
    start_time: SystemTime,
    strings: FxIndexSet<String>,
    timestamp_key: StringId,
    timestamped_samples: HashMap<Sample, Vec<(Timestamp, Vec<i64>)>>,
    upscaling_rules: UpscalingRules,
}

pub struct Endpoints {
    mappings: FxIndexMap<u64, StringId>,
    local_root_span_id_label: StringId,
    endpoint_label: StringId,
    stats: ProfiledEndpointsStats,
}

#[derive(Default)]
pub struct UpscalingRules {
    rules: FxIndexMap<(StringId, StringId), Vec<UpscalingRule>>,
    // this is just an optimization in the case where we check collisions (when adding
    // a by-value rule) against by-label rules
    // 32 should be enough for the size of the bitmap
    offset_modified_by_bylabel_rule: bitmaps::Bitmap<32>,
}

impl UpscalingRules {
    pub fn add(&mut self, label_name_id: StringId, label_value_id: StringId, rule: UpscalingRule) {
        // fill the bitmap for by-label rules
        if !label_name_id.is_zero() || !label_value_id.is_zero() {
            rule.values_offset.iter().for_each(|offset| {
                self.offset_modified_by_bylabel_rule.set(*offset, true);
            })
        }
        match self.rules.get_index_of(&(label_name_id, label_value_id)) {
            None => {
                let rules = vec![rule];
                self.rules.insert((label_name_id, label_value_id), rules);
            }
            Some(index) => {
                let (_, rules) = self
                    .rules
                    .get_index_mut(index)
                    .expect("Already existing rules");
                rules.push(rule);
            }
        }
    }

    pub fn get(&self, k: &(StringId, StringId)) -> Option<&Vec<UpscalingRule>> {
        self.rules.get(k)
    }

    fn check_collisions(
        &self,
        values_offset: &[usize],
        label_name: (&str, StringId),
        label_value: (&str, StringId),
        upscaling_info: &UpscalingInfo,
    ) -> anyhow::Result<()> {
        // Check for duplicates
        fn is_overlapping(v1: &[usize], v2: &[usize]) -> bool {
            v1.iter().any(|x| v2.contains(x))
        }

        fn vec_to_string(v: &[usize]) -> String {
            format!("{:?}", v)
        }

        let colliding_rule = match self.rules.get(&(label_name.1, label_value.1)) {
            Some(rules) => rules
                .iter()
                .find(|rule| is_overlapping(&rule.values_offset, values_offset)),
            None => None,
        };

        anyhow::ensure!(
            colliding_rule.is_none(),
            "There are dupicated by-label rules for the same label name: {} and label value: {} with at least one value offset in common.\n\
            Existing values offset(s) {}, new rule values offset(s) {}.\n\
            Existing upscaling info: {}, new rule upscaling info: {}",
            vec_to_string(&colliding_rule.unwrap().values_offset), vec_to_string(values_offset),
            label_name.0, label_value.0,
            upscaling_info, colliding_rule.unwrap().upscaling_info
        );

        // if we are adding a by-value rule, we need to check against
        // all by-label rules for collisions
        if label_name.1.is_zero() && label_value.1.is_zero() {
            let collision_offset = values_offset
                .iter()
                .find(|offset| self.offset_modified_by_bylabel_rule.get(**offset));

            anyhow::ensure!(
                collision_offset.is_none(),
                "The by-value rule is collinding with at least one by-label rule at offset {}\n\
                by-value rule values offset(s) {}",
                collision_offset.unwrap(),
                vec_to_string(values_offset)
            )
        } else if let Some(rules) = self.rules.get(&(StringId::ZERO, StringId::ZERO)) {
            let collide_with_byvalue_rule = rules
                .iter()
                .find(|rule| is_overlapping(&rule.values_offset, values_offset));
            anyhow::ensure!(collide_with_byvalue_rule.is_none(),
                "The by-label rule (label name {}, label value {}) is colliding with a by-value rule on values offsets\n\
                Existing values offset(s) {}, new rule values offset(s) {}",
                label_name.0, label_value.0, vec_to_string(&collide_with_byvalue_rule.unwrap().values_offset),
                vec_to_string(values_offset))
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

#[derive(Default)]
pub struct ProfileBuilder<'a> {
    period: Option<api::Period<'a>>,
    sample_types: Vec<api::ValueType<'a>>,
    start_time: Option<SystemTime>,
}

impl<'a> ProfileBuilder<'a> {
    pub const fn new() -> Self {
        ProfileBuilder {
            period: None,
            sample_types: Vec::new(),
            start_time: None,
        }
    }

    pub fn period(mut self, period: Option<api::Period<'a>>) -> Self {
        self.period = period;
        self
    }

    pub fn sample_types(mut self, sample_types: Vec<api::ValueType<'a>>) -> Self {
        self.sample_types = sample_types;
        self
    }

    pub fn start_time(mut self, start_time: Option<SystemTime>) -> Self {
        self.start_time = start_time;
        self
    }

    pub fn build(self) -> Profile {
        let mut profile = Profile::new(self.start_time.unwrap_or_else(SystemTime::now));

        profile.sample_types = self
            .sample_types
            .iter()
            .map(|vt| ValueType {
                r#type: profile.intern(vt.r#type),
                unit: profile.intern(vt.unit),
            })
            .collect();

        if let Some(period) = self.period {
            profile.period = Some((
                period.value,
                ValueType {
                    r#type: profile.intern(period.r#type.r#type),
                    unit: profile.intern(period.r#type.unit),
                },
            ));
        };

        profile
    }
}

trait Dedup<T: Item> {
    /// Deduplicate the Item and return its associated Id.
    /// # Panics
    /// Panics if the number of items overflows the storage capabilities of
    /// the associated Id type.
    fn dedup(&mut self, item: T) -> <T as Item>::Id;
}

impl<T: Item> Dedup<T> for FxIndexSet<T> {
    fn dedup(&mut self, item: T) -> <T as Item>::Id {
        let (id, _) = self.insert_full(item);
        <T as Item>::Id::from_offset(id)
    }
}

fn to_pprof_vec<T: PprofItem>(collection: &FxIndexSet<T>) -> Vec<T::PprofMessage> {
    collection
        .iter()
        .enumerate()
        .map(|(index, item)| item.to_pprof(<T as Item>::Id::from_offset(index)))
        .collect()
}

pub struct EncodedProfile {
    pub start: SystemTime,
    pub end: SystemTime,
    pub buffer: Vec<u8>,
    pub endpoints_stats: ProfiledEndpointsStats,
}

impl Endpoints {
    pub fn new() -> Self {
        Self {
            mappings: Default::default(),
            local_root_span_id_label: Default::default(),
            endpoint_label: Default::default(),
            stats: Default::default(),
        }
    }
}

impl Default for Endpoints {
    fn default() -> Self {
        Self::new()
    }
}

// For testing and debugging purposes
impl Profile {
    pub fn only_for_testing_num_aggregated_samples(&self) -> usize {
        self.aggregated_samples.len()
    }
    pub fn only_for_testing_num_timestamped_samples(&self) -> usize {
        self.timestamped_samples.len()
    }
}

impl Profile {
    /// Creates a profile with `start_time`.
    /// Initializes the string table to hold:
    ///  - "" (the empty string)
    ///  - "local root span id"
    ///  - "trace endpoint"
    /// All other fields are default.
    pub fn new(start_time: SystemTime) -> Self {
        /* Do not use Profile's default() impl here or it will cause a stack
         * overflow, since that default impl calls this method.
         */
        let mut profile = Self {
            aggregated_samples: Default::default(),
            endpoints: Default::default(),
            functions: Default::default(),
            locations: Default::default(),
            mappings: Default::default(),
            period: None,
            sample_types: vec![],
            stack_traces: Default::default(),
            start_time,
            strings: Default::default(),
            timestamp_key: Default::default(),
            timestamped_samples: Default::default(),
            upscaling_rules: Default::default(),
        };

        // Ensure the empty string is the first inserted item and has a 0 id.
        let _id = profile.intern("");
        debug_assert!(_id == StringId::ZERO);

        profile.endpoints.local_root_span_id_label = profile.intern("local root span id");
        profile.endpoints.endpoint_label = profile.intern("trace endpoint");
        profile.timestamp_key = profile.intern("end_timestamp_ns");
        profile
    }

    #[cfg(test)]
    fn interned_strings_count(&self) -> usize {
        self.strings.len()
    }

    /// Interns the `str` as a string, returning the id in the string table.
    /// The empty string is guaranteed to have an id of [StringId::ZERO].
    fn intern(&mut self, item: &str) -> StringId {
        // For performance, delay converting the [&str] to a [String] until
        // after it has been determined to not exist in the set. This avoids
        // temporary allocations.
        let index = match self.strings.get_index_of(item) {
            Some(index) => index,
            None => {
                let (index, _inserted) = self.strings.insert_full(item.into());
                // This wouldn't make any sense; the item couldn't be found so
                // we try to insert it, but suddenly it exists now?
                debug_assert!(_inserted);
                index
            }
        };
        StringId::from_offset(index)
    }

    pub fn builder<'a>() -> ProfileBuilder<'a> {
        ProfileBuilder::new()
    }

    fn add_stacktrace(&mut self, locations: Vec<LocationId>) -> StackTraceId {
        self.stack_traces.dedup(StackTrace { locations })
    }

    fn get_stacktrace(&self, st: StackTraceId) -> &StackTrace {
        self.stack_traces
            .get_index(st.to_raw_id())
            .expect("StackTraceId {st} to exist in profile")
    }

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

    fn add_location(&mut self, location: &api::Location) -> LocationId {
        let mapping_id = self.add_mapping(&location.mapping);
        let lines: Vec<Line> = location
            .lines
            .iter()
            .map(|line| {
                let function_id = self.add_function(&line.function);
                let line1 = line.line;
                Line {
                    function_id,
                    line: line1,
                }
            })
            .collect();

        self.locations.dedup(Location {
            mapping_id,
            address: location.address,
            lines,
            is_folded: location.is_folded,
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

    pub fn add(&mut self, sample: api::Sample) -> anyhow::Result<()> {
        anyhow::ensure!(
            sample.values.len() == self.sample_types.len(),
            "expected {} sample types, but sample had {} sample types",
            self.sample_types.len(),
            sample.values.len(),
        );

        let values = sample.values.clone();
        let (labels, local_root_span_id_label_offset, timestamp) =
            self.extract_sample_labels(&sample)?;

        let locations = sample
            .locations
            .iter()
            .map(|l| self.add_location(l))
            .collect();

        let stacktrace = self.add_stacktrace(locations);
        let s = Sample {
            stacktrace,
            labels,
            local_root_span_id_label_offset,
        };

        if let Some(ts) = timestamp {
            match self.timestamped_samples.get_mut(&s) {
                None => {
                    let series = vec![(ts, values)];
                    self.timestamped_samples.insert(s, series);
                }
                // Repeated timestamps are unlikely but possible.
                // We choose to record each as separate observations, and
                // allow the backend to decide what to do.
                Some(series) => series.push((ts, values)),
            }
        } else {
            match self.aggregated_samples.get_mut(&s) {
                None => {
                    self.aggregated_samples.insert(s, values);
                }
                Some(v) => v.iter_mut().zip(values).for_each(|(a, b)| *a += b),
            }
        };
        Ok(())
    }

    /// Validates labels and converts them to the internal representation.
    /// Extracts out the timestamp label, if it exists.
    /// Also tracks the index of the label with key "local root span id".
    fn extract_sample_labels(
        &mut self,
        sample: &api::Sample,
    ) -> anyhow::Result<(Vec<Label>, Option<usize>, Option<Timestamp>)> {
        let mut labels: Vec<Label> = Vec::with_capacity(sample.labels.len());
        let mut local_root_span_id_label_offset: Option<usize> = None;
        let mut timestamp: Option<Timestamp> = None;

        for label in sample.labels.iter() {
            let key = self.intern(label.key);

            // The current API stores timestamps on a label.
            // The internal representation stores them in the timestamped_samples map.
            // Extract them from the labels, and pass them on to put into the map
            if key == self.timestamp_key {
                anyhow::ensure!(
                    label.str.is_none(),
                    "the label \"{}\" must be sent as a number, not string {}",
                    label.str.unwrap(),
                    label.key
                );
                anyhow::ensure!(label.num != 0, "the label \"{}\" must not be 0", label.key);
                anyhow::ensure!(label.num_unit.is_none(), "Timestamps with label '{}' are always nanoseconds and do not take a unit: found '{}'", label.key, label.num_unit.unwrap());

                timestamp = Some(NonZeroI64::new(label.num).unwrap());
                // Once the timestamp is extracted, we remove it from the labels.
                // This allows `Sample` with the same values other than the timestamp to dedup.
                continue;
            }

            if key == self.endpoints.local_root_span_id_label {
                // Panic: if the label.str isn't 0, then str must have been provided.
                anyhow::ensure!(
                    label.str.is_none(),
                    "the label \"local root span id\" must be sent as a number, not string {}",
                    label.str.unwrap()
                );
                anyhow::ensure!(
                    label.num != 0,
                    "the label \"local root span id\" must not be 0"
                );
                anyhow::ensure!(
                    local_root_span_id_label_offset.is_none(),
                    "only one label per sample can have the key \"local root span id\", found two: {:?}, {}",
                    labels[local_root_span_id_label_offset.unwrap()], label.num
                );
                local_root_span_id_label_offset = Some(labels.len());
            }

            let internal_label = if let Some(s) = label.str {
                let str = self.intern(s);
                Label::str(key, str)
            } else {
                let num = label.num;
                let num_unit = label.num_unit.map(|s| self.intern(s));
                Label::num(key, num, num_unit)
            };

            // If you refactor this push, ensure the local_root_span_id_label_offset is correct.
            labels.push(internal_label);
        }
        Ok((labels, local_root_span_id_label_offset, timestamp))
    }

    fn extract_api_sample_types(&self) -> Option<Vec<api::ValueType>> {
        let mut sample_types: Vec<api::ValueType> = Vec::with_capacity(self.sample_types.len());
        for sample_type in self.sample_types.iter() {
            sample_types.push(api::ValueType {
                r#type: self.get_string(sample_type.r#type),
                unit: self.get_string(sample_type.unit),
            })
        }
        Some(sample_types)
    }

    /// Resets all data except the sample types and period. Returns the
    /// previous Profile on success.
    pub fn reset(&mut self, start_time: Option<SystemTime>) -> Option<Profile> {
        /* We have to map over the types because the order of the strings is
         * not generally guaranteed, so we can't just copy the underlying
         * structures.
         */
        let sample_types: Vec<api::ValueType> = self.extract_api_sample_types()?;

        let period = self.period.map(|t| api::Period {
            r#type: api::ValueType {
                r#type: self.get_string(t.1.r#type),
                unit: self.get_string(t.1.unit),
            },
            value: t.0,
        });

        let mut profile = ProfileBuilder::new()
            .sample_types(sample_types)
            .period(period)
            .start_time(start_time)
            .build();

        std::mem::swap(&mut *self, &mut profile);
        Some(profile)
    }

    /// Add the endpoint data to the endpoint mappings.
    /// The `endpoint` string will be interned.
    pub fn add_endpoint(&mut self, local_root_span_id: u64, endpoint: Cow<str>) {
        let interned_endpoint = self.intern(endpoint.as_ref());

        self.endpoints
            .mappings
            .insert(local_root_span_id, interned_endpoint);
    }

    pub fn add_endpoint_count(&mut self, endpoint: Cow<str>, value: i64) {
        self.endpoints
            .stats
            .add_endpoint_count(endpoint.into_owned(), value);
    }

    pub fn add_upscaling_rule(
        &mut self,
        offset_values: &[usize],
        label_name: &str,
        label_value: &str,
        upscaling_info: UpscalingInfo,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            offset_values.iter().all(|x| x < &self.sample_types.len()),
            "Invalid offset. Highest expected offset: {}",
            self.sample_types.len() - 1
        );

        let label_name_id = self.intern(label_name);
        let label_value_id = self.intern(label_value);

        let mut new_values_offset = offset_values.to_vec();
        new_values_offset.sort_unstable();

        self.upscaling_rules.check_collisions(
            &new_values_offset,
            (label_name, label_name_id),
            (label_value, label_value_id),
            &upscaling_info,
        )?;

        upscaling_info.check_validity(self.sample_types.len())?;

        let rule = UpscalingRule {
            values_offset: new_values_offset,
            upscaling_info,
        };

        self.upscaling_rules
            .add(label_name_id, label_value_id, rule);

        Ok(())
    }

    /// Serialize the aggregated profile, adding the end time and duration.
    /// # Arguments
    /// * `end_time` - Optional end time of the profile. Passing None will use the current time.
    /// * `duration` - Optional duration of the profile. Passing None will try to calculate the
    ///                duration based on the end time minus the start time, but under anomalous
    ///                conditions this may fail as system clocks can be adjusted. The programmer
    ///                may also accidentally pass an earlier time. The duration will be set to zero
    ///                these cases.
    pub fn serialize(
        &self,
        end_time: Option<SystemTime>,
        duration: Option<Duration>,
    ) -> anyhow::Result<EncodedProfile> {
        let end = end_time.unwrap_or_else(SystemTime::now);
        let start = self.start_time;
        let mut profile: pprof::Profile = self.try_into()?;

        profile.duration_nanos = duration
            .unwrap_or_else(|| {
                end.duration_since(start).unwrap_or({
                    // Let's not throw away the whole profile just because the clocks were wrong.
                    // todo: log that the clock went backward (or programmer mistake).
                    Duration::ZERO
                })
            })
            .as_nanos()
            .min(i64::MAX as u128) as i64;

        let mut buffer: Vec<u8> = Vec::new();
        profile.encode(&mut buffer)?;

        Ok(EncodedProfile {
            start,
            end,
            buffer,
            endpoints_stats: self.endpoints.stats.clone(),
        })
    }

    pub fn get_string(&self, id: StringId) -> &str {
        self.strings
            .get_index(id.to_offset())
            .expect("StringId to have a valid interned index")
    }

    /// Fetches the endpoint information for the label. There may be errors,
    /// but there may also be no endpoint information for a given endpoint.
    /// Hence, the return type of Result<Option<_>, _>.
    fn get_endpoint_for_label(&self, label: &Label) -> anyhow::Result<Option<StringId>> {
        anyhow::ensure!(
            label.get_key() == self.endpoints.local_root_span_id_label,
            "bug: get_endpoint_for_label should only be called on labels with the key \"local root span id\", called on label with key \"{}\"",
            self.get_string(label.get_key())
        );

        anyhow::ensure!(
            label.has_num_value(),
            "the local root span id label value must be sent as a number, not a string, given {:?}",
            label
        );

        let local_root_span_id: u64 = if let LabelValue::Num { num, .. } = label.get_value() {
            // Manually specify the type here to be sure we're transmuting an
            // i64 and not a &i64.
            let id: i64 = *num;
            // Safety: the value is a u64, but pprof only has signed values, so we
            // transmute it; the backend does the same.
            unsafe { std::intrinsics::transmute(id) }
        } else {
            return Err(anyhow::format_err!("the local root span id label value must be sent as a number, not a string, given {:?}",
            label));
        };

        Ok(self
            .endpoints
            .mappings
            .get(&local_root_span_id)
            .map(StringId::clone))
    }

    // TODO: Consider whether to use the internal Label here instead
    fn upscale_values(&self, values: &[i64], labels: &[pprof::Label]) -> anyhow::Result<Vec<i64>> {
        let mut new_values = values.to_vec();

        if !self.upscaling_rules.is_empty() {
            let mut values_to_update: Vec<usize> = vec![0; self.sample_types.len()];

            // get bylabel rules first (if any)
            let mut group_of_rules = labels
                .iter()
                .filter_map(|label| {
                    self.upscaling_rules
                        .get(&(StringId::new(label.key), StringId::new(label.str)))
                })
                .collect::<Vec<&Vec<UpscalingRule>>>();

            // get byvalue rules if any
            if let Some(byvalue_rules) = self.upscaling_rules.get(&(StringId::ZERO, StringId::ZERO))
            {
                group_of_rules.push(byvalue_rules);
            }

            // check for collision(s)
            group_of_rules.iter().for_each(|rules| {
                rules.iter().for_each(|rule| {
                    rule.values_offset
                        .iter()
                        .for_each(|offset| values_to_update[*offset] += 1)
                })
            });

            anyhow::ensure!(
                values_to_update.iter().all(|v| *v < 2),
                "Multiple rules modifying the same offset for this sample"
            );

            group_of_rules.iter().for_each(|rules| {
                rules.iter().for_each(|rule| {
                    let scale = rule.compute_scale(values);
                    rule.values_offset.iter().for_each(|offset| {
                        new_values[*offset] = (new_values[*offset] as f64 * scale).round() as i64
                    })
                })
            });
        }

        Ok(new_values)
    }

    fn translate_and_enrich_sample_labels(
        &self,
        sample: &Sample,
    ) -> anyhow::Result<Vec<pprof::Label>> {
        let mut labels: Vec<_> = sample.labels.iter().map(pprof::Label::from).collect();
        if let Some(offset) = sample.local_root_span_id_label_offset {
            // Safety: this offset was created internally and isn't be mutated.
            let lrsi_label = unsafe { sample.labels.get_unchecked(offset) };
            if let Some(endpoint_value_id) = self.get_endpoint_for_label(lrsi_label)? {
                labels.push(Label::str(self.endpoints.endpoint_label, endpoint_value_id).into());
            }
        }
        Ok(labels)
    }

    /// Given a sample, and a set of observations at different timestamps,
    /// expand to one Sample for each timestamp.
    fn expand_timestamped_sample(
        &self,
        sample: &Sample,
        observations: &[(Timestamp, Vec<i64>)],
    ) -> anyhow::Result<Vec<pprof::Sample>> {
        // Clone the labels, but enrich them with endpoint profiling.
        let labels = self.translate_and_enrich_sample_labels(sample)?;
        let location_ids: Vec<_> = self
            .get_stacktrace(sample.stacktrace)
            .locations
            .iter()
            .map(Id::to_raw_id)
            .collect();

        observations
            .iter()
            .map(|(timestamp, values)| {
                let new_values: Vec<i64> = self.upscale_values(values.as_ref(), &labels)?;
                let mut labels = labels.clone();

                // pprof uses a label to store the timestamp so put it there
                labels.push(Label::num(self.timestamp_key, timestamp.get(), None).into());

                Ok(pprof::Sample {
                    location_ids: location_ids.clone(),
                    values: new_values,
                    labels,
                })
            })
            .collect()
    }
}

impl TryFrom<&Profile> for pprof::Profile {
    type Error = anyhow::Error;

    fn try_from(profile: &Profile) -> anyhow::Result<pprof::Profile> {
        let (period, period_type) = match profile.period {
            Some(tuple) => (tuple.0, Some(tuple.1)),
            None => (0, None),
        };

        /* Rust pattern: inverting Vec<Result<T,E>> into Result<Vec<T>, E> error with .collect:
         * https://doc.rust-lang.org/rust-by-example/error/iter_result.html#fail-the-entire-operation-with-collect
         */
        let timestamped_samples: anyhow::Result<Vec<Vec<pprof::Sample>>> = profile
            .timestamped_samples
            .iter()
            .map(|(sample, observations)| profile.expand_timestamped_sample(sample, observations))
            .collect();

        let aggregated_samples: anyhow::Result<Vec<pprof::Sample>> = profile
            .aggregated_samples
            .iter()
            .map(|(sample, values)| {
                let labels = profile.translate_and_enrich_sample_labels(sample)?;

                let location_ids: Vec<_> = profile
                    .get_stacktrace(sample.stacktrace)
                    .locations
                    .iter()
                    .map(Id::to_raw_id)
                    .collect();
                let new_values: Vec<i64> =
                    profile.upscale_values(values.as_ref(), labels.as_ref())?;
                Ok(pprof::Sample {
                    location_ids,
                    values: new_values,
                    labels,
                })
            })
            .collect();

        let samples: Vec<_> = timestamped_samples?
            .into_iter()
            .flatten()
            .chain(aggregated_samples?)
            .collect();

        Ok(pprof::Profile {
            sample_types: profile
                .sample_types
                .iter()
                .map(pprof::ValueType::from)
                .collect(),
            samples,
            mappings: to_pprof_vec(&profile.mappings),
            locations: to_pprof_vec(&profile.locations),
            functions: to_pprof_vec(&profile.functions),
            string_table: profile.strings.iter().map(Into::into).collect(),
            time_nanos: profile
                .start_time
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |duration| {
                    duration.as_nanos().min(i64::MAX as u128) as i64
                }),
            period,
            period_type: period_type.map(pprof::ValueType::from),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod api_test {

    use super::*;
    use std::{borrow::Cow, collections::HashMap};

    #[test]
    fn interning() {
        let sample_types = vec![api::ValueType {
            r#type: "samples",
            unit: "count",
        }];
        let mut profiles = Profile::builder().sample_types(sample_types).build();

        let expected_id = StringId::from_offset(profiles.interned_strings_count());

        let string = "a";
        let id1 = profiles.intern(string);
        let id2 = profiles.intern(string);

        assert_eq!(id1, id2);
        assert_eq!(id1, expected_id);
    }

    #[test]
    fn api() {
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
                lines: vec![api::Line {
                    function: api::Function {
                        name: "phpinfo",
                        system_name: "phpinfo",
                        filename: "index.php",
                        start_line: 0,
                    },
                    line: 0,
                }],
                ..Default::default()
            },
            api::Location {
                mapping,
                lines: vec![api::Line {
                    function: index,
                    line: 3,
                }],
                ..Default::default()
            },
        ];

        let mut profile = Profile::builder().sample_types(sample_types).build();
        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 0);

        profile
            .add(api::Sample {
                locations,
                values: vec![1, 10000],
                labels: vec![],
            })
            .expect("add to succeed");

        assert_eq!(profile.only_for_testing_num_aggregated_samples(), 1);
    }

    fn provide_distinct_locations() -> Profile {
        let sample_types = vec![api::ValueType {
            r#type: "samples",
            unit: "count",
        }];

        let main_lines = vec![api::Line {
            function: api::Function {
                name: "{main}",
                system_name: "{main}",
                filename: "index.php",
                start_line: 0,
            },
            line: 0,
        }];

        let test_lines = vec![api::Line {
            function: api::Function {
                name: "test",
                system_name: "test",
                filename: "index.php",
                start_line: 3,
            },
            line: 0,
        }];

        let timestamp_lines = vec![api::Line {
            function: api::Function {
                name: "test",
                system_name: "test",
                filename: "index.php",
                start_line: 3,
            },
            line: 0,
        }];

        let mapping = api::Mapping {
            filename: "php",
            ..Default::default()
        };

        let main_locations = vec![api::Location {
            mapping,
            lines: main_lines,
            ..Default::default()
        }];
        let test_locations = vec![api::Location {
            mapping,
            lines: test_lines,
            ..Default::default()
        }];
        let timestamp_locations = vec![api::Location {
            mapping,
            lines: timestamp_lines,
            ..Default::default()
        }];

        let values: Vec<i64> = vec![1];
        let mut labels = vec![api::Label {
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

        labels.push(api::Label {
            key: "end_timestamp_ns",
            num: 42,
            ..Default::default()
        });
        let timestamp_sample = api::Sample {
            locations: timestamp_locations,
            values,
            labels,
        };

        let mut profile = Profile::builder().sample_types(sample_types).build();
        assert_eq!(profile.aggregated_samples.len(), 0);

        profile.add(main_sample).expect("profile to not be full");
        assert_eq!(profile.aggregated_samples.len(), 1);

        profile.add(test_sample).expect("profile to not be full");
        assert_eq!(profile.aggregated_samples.len(), 2);

        assert_eq!(profile.timestamped_samples.len(), 0);
        profile
            .add(timestamp_sample)
            .expect("profile to not be full");
        assert_eq!(profile.timestamped_samples.len(), 1);
        profile
    }

    #[test]
    fn impl_from_profile_for_pprof_profile() {
        let locations = provide_distinct_locations();
        let profile = pprof::Profile::try_from(&locations).unwrap();

        assert_eq!(profile.samples.len(), 3);
        assert_eq!(profile.mappings.len(), 1);
        assert_eq!(profile.locations.len(), 2);
        assert_eq!(profile.functions.len(), 2);

        for (index, mapping) in profile.mappings.iter().enumerate() {
            assert_eq!((index + 1) as u64, mapping.id);
        }

        for (index, location) in profile.locations.iter().enumerate() {
            assert_eq!((index + 1) as u64, location.id);
        }

        for (index, function) in profile.functions.iter().enumerate() {
            assert_eq!((index + 1) as u64, function.id);
        }

        let sample = profile.samples.get(0).expect("index 0 to exist");
        assert_eq!(sample.labels.len(), 2);
        let label = sample.labels.get(0).expect("index 0 to exist");
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

        let sample = profile.samples.get(1).expect("index 1 to exist");
        assert_eq!(sample.labels.len(), 1);
        let label = sample.labels.get(0).expect("index 0 to exist");
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
    }

    #[test]
    fn reset() {
        let mut profile = provide_distinct_locations();
        /* This set of asserts is to make sure it's a non-empty profile that we
         * are working with so that we can test that reset works.
         */
        assert!(!profile.functions.is_empty());
        assert!(!profile.locations.is_empty());
        assert!(!profile.mappings.is_empty());
        assert!(!profile.aggregated_samples.is_empty());
        assert!(!profile.timestamped_samples.is_empty());
        assert!(!profile.sample_types.is_empty());
        assert!(profile.period.is_none());
        assert!(profile.endpoints.mappings.is_empty());
        assert!(profile.endpoints.stats.is_empty());

        let prev = profile.reset(None).expect("reset to succeed");

        // These should all be empty now
        assert!(profile.functions.is_empty());
        assert!(profile.locations.is_empty());
        assert!(profile.mappings.is_empty());
        assert!(profile.aggregated_samples.is_empty());
        assert!(profile.timestamped_samples.is_empty());
        assert!(profile.endpoints.mappings.is_empty());
        assert!(profile.endpoints.stats.is_empty());
        assert!(profile.upscaling_rules.is_empty());

        assert_eq!(profile.period, prev.period);
        assert_eq!(profile.sample_types, prev.sample_types);

        // The string table should have at least the empty string.
        assert!(!profile.strings.is_empty());
        assert_eq!("", profile.get_string(StringId::ZERO));
    }

    #[test]
    fn reset_period() {
        /* The previous test (reset) checked quite a few properties already, so
         * this one will focus only on the period.
         */
        let mut profile = provide_distinct_locations();

        let period = Some((
            10_000_000,
            ValueType {
                r#type: profile.intern("wall-time"),
                unit: profile.intern("nanoseconds"),
            },
        ));
        profile.period = period;

        let prev = profile.reset(None).expect("reset to succeed");
        assert_eq!(period, prev.period);

        // Resolve the string values to check that they match (their string
        // table offsets may not match).
        let (value, period_type) = profile.period.expect("profile to have a period");
        assert_eq!(value, period.unwrap().0);
        assert_eq!(profile.get_string(period_type.r#type), "wall-time");
        assert_eq!(profile.get_string(period_type.unit), "nanoseconds");
    }

    #[test]
    fn adding_local_root_span_id_with_string_value_fails() {
        let sample_types = vec![api::ValueType {
            r#type: "wall-time",
            unit: "nanoseconds",
        }];

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

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

        assert!(profile.add(sample).is_err());
    }

    #[test]
    fn lazy_endpoints() {
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

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

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

        profile.add(sample1).expect("add to success");

        profile.add(sample2).expect("add to success");

        profile.add_endpoint(10, Cow::from("my endpoint"));

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();
        assert_eq!(serialized_profile.samples.len(), 2);
        let samples = serialized_profile.sorted_samples();

        let s1 = samples.get(0).expect("sample");

        // The trace endpoint label should be added to the first sample
        assert_eq!(s1.labels.len(), 3);

        let l1 = s1.labels.get(0).expect("label");

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

        // The trace endpoint label shouldn't be added to second sample because the span id doesn't match
        assert_eq!(s2.labels.len(), 2);
    }

    #[test]
    fn endpoint_counts_empty_test() {
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

        let profile: Profile = Profile::builder().sample_types(sample_types).build();

        let encoded_profile = profile
            .serialize(None, None)
            .expect("Unable to encode/serialize the profile");

        let endpoints_stats = encoded_profile.endpoints_stats;
        assert!(endpoints_stats.is_empty());
    }

    #[test]
    fn endpoint_counts_test() {
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

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

        let one_endpoint = "my endpoint";
        profile.add_endpoint_count(Cow::from(one_endpoint), 1);
        profile.add_endpoint_count(Cow::from(one_endpoint), 1);

        let second_endpoint = "other endpoint";
        profile.add_endpoint_count(Cow::from(second_endpoint), 1);

        let encoded_profile = profile
            .serialize(None, None)
            .expect("Unable to encode/serialize the profile");

        let endpoints_stats = encoded_profile.endpoints_stats;

        let mut count: HashMap<String, i64> = HashMap::new();
        count.insert(one_endpoint.to_string(), 2);
        count.insert(second_endpoint.to_string(), 1);

        let expected_endpoints_stats = ProfiledEndpointsStats::from(count);

        assert_eq!(endpoints_stats, expected_endpoints_stats);
    }

    #[test]
    fn local_root_span_id_label_cannot_occur_more_than_once() {
        let sample_types = vec![api::ValueType {
            r#type: "wall-time",
            unit: "nanoseconds",
        }];

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

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

        profile.add(sample).unwrap_err();
    }

    #[test]
    fn test_no_upscaling_if_no_rules() {
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

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

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

        profile.add(sample1).expect("add to success");

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();

        let first = serialized_profile.samples.get(0).expect("one sample");

        assert_eq!(first.values[0], 1);
        assert_eq!(first.values[1], 10000);
    }

    fn create_samples_types() -> Vec<api::ValueType<'static>> {
        vec![
            api::ValueType {
                r#type: "samples",
                unit: "count",
            },
            api::ValueType {
                r#type: "wall-time",
                unit: "nanoseconds",
            },
            api::ValueType {
                r#type: "cpu-time",
                unit: "nanoseconds",
            },
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

        let mut profile = Profile::builder().sample_types(sample_types).build();

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![0, 10000, 42],
            labels: vec![],
        };

        profile.add(sample1).expect("add to success");

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        let values_offset = vec![0];
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();

        let first = serialized_profile.samples.get(0).expect("one sample");

        assert_eq!(first.values, vec![0, 10000, 42]);
    }

    #[test]
    fn test_upscaling_by_value_on_one_value() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
            labels: vec![],
        };

        profile.add(sample1).expect("add to success");

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.7 };
        let values_offset = vec![0];
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();

        let first = serialized_profile.samples.get(0).expect("one sample");

        assert_eq!(first.values, vec![3, 10000, 42]);
    }

    #[test]
    fn test_upscaling_by_value_on_one_value_with_poisson() {
        let sample_types = create_samples_types();

        let mut profile = Profile::builder().sample_types(sample_types).build();

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 16, 29],
            labels: vec![],
        };

        profile.add(sample1).expect("add to success");

        let upscaling_info = UpscalingInfo::Poisson {
            sum_value_offset: 1,
            count_value_offset: 2,
            sampling_distance: 10,
        };
        let values_offset: Vec<usize> = vec![1];
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();

        let first = serialized_profile.samples.get(0).expect("one sample");

        assert_eq!(first.values, vec![1, 298, 29]);
    }

    #[test]
    fn test_upscaling_by_value_on_zero_value_with_poisson() {
        let sample_types = create_samples_types();

        let mut profile = Profile::builder().sample_types(sample_types).build();

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 16, 0],
            labels: vec![],
        };

        profile.add(sample1).expect("add to success");

        let upscaling_info = UpscalingInfo::Poisson {
            sum_value_offset: 1,
            count_value_offset: 2,
            sampling_distance: 10,
        };
        let values_offset: Vec<usize> = vec![1];
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();

        let first = serialized_profile.samples.get(0).expect("one sample");

        assert_eq!(first.values, vec![1, 16, 0]);
    }

    #[test]
    fn test_cannot_add_a_rule_with_invalid_poisson_info() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 16, 0],
            labels: vec![],
        };

        profile.add(sample1).expect("add to success");

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

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 21],
            labels: vec![],
        };

        let main_lines = vec![api::Line {
            function: api::Function {
                name: "{main}",
                system_name: "{main}",
                filename: "index.php",
                start_line: 0,
            },
            line: 0,
        }];

        let mapping = api::Mapping {
            filename: "php",
            ..Default::default()
        };

        let main_locations = vec![api::Location {
            mapping,
            lines: main_lines,
            ..Default::default()
        }];

        let sample2 = api::Sample {
            locations: main_locations,
            values: vec![5, 24, 99],
            labels: vec![],
        };

        profile.add(sample1).expect("add to success");
        profile.add(sample2).expect("add to success");

        // upscale the first value and the last one
        let values_offset: Vec<usize> = vec![0, 2];

        let upscaling_info = UpscalingInfo::Proportional { scale: 2.0 };
        profile
            .add_upscaling_rule(values_offset.as_slice(), "", "", upscaling_info)
            .expect("Rule added");

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();
        let samples = serialized_profile.sorted_samples();
        let first = samples.get(0).expect("first sample");

        assert_eq!(first.values, vec![2, 10000, 42]);

        let second = samples.get(1).expect("second sample");

        assert_eq!(second.values, vec![10, 24, 198]);
    }

    #[test]
    fn test_upscaling_by_value_on_two_value_with_two_rules() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 21],
            labels: vec![],
        };

        let main_lines = vec![api::Line {
            function: api::Function {
                name: "{main}",
                system_name: "{main}",
                filename: "index.php",
                start_line: 0,
            },
            line: 0,
        }];

        let mapping = api::Mapping {
            filename: "php",
            ..Default::default()
        };

        let main_locations = vec![api::Location {
            mapping,
            lines: main_lines,
            ..Default::default()
        }];

        let sample2 = api::Sample {
            locations: main_locations,
            values: vec![5, 24, 99],
            labels: vec![],
        };

        profile.add(sample1).expect("add to success");
        profile.add(sample2).expect("add to success");

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

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();
        let samples = serialized_profile.sorted_samples();
        let first = samples.get(0).expect("first sample");

        assert_eq!(first.values, vec![2, 10000, 105]);

        let second = samples.get(1).expect("second sample");

        assert_eq!(second.values, vec![10, 24, 495]);
    }

    #[test]
    fn test_no_upscaling_by_label_if_no_match() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

        let id_label = create_label("my_label", Some("coco"));

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
            labels: vec![id_label],
        };

        profile.add(sample1).expect("add to success");

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

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();

        let first = serialized_profile.samples.get(0).expect("one sample");

        assert_eq!(first.values, vec![1, 10000, 42]);
    }

    #[test]
    fn test_upscaling_by_label_on_one_value() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

        let id_label = create_label("my label", Some("coco"));

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
            labels: vec![id_label],
        };

        profile.add(sample1).expect("add to success");

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

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();

        let first = serialized_profile.samples.get(0).expect("one sample");

        assert_eq!(first.values, vec![2, 10000, 42]);
    }

    #[test]
    fn test_upscaling_by_label_on_only_sample_out_of_two() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

        let id_label = create_label("my label", Some("coco"));

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
            labels: vec![id_label],
        };

        let main_lines = vec![api::Line {
            function: api::Function {
                name: "{main}",
                system_name: "{main}",
                filename: "index.php",
                start_line: 0,
            },
            line: 0,
        }];

        let mapping = api::Mapping {
            filename: "php",
            ..Default::default()
        };

        let main_locations = vec![api::Location {
            mapping,
            lines: main_lines,
            ..Default::default()
        }];

        let sample2 = api::Sample {
            locations: main_locations,
            values: vec![5, 24, 99],
            labels: vec![],
        };

        profile.add(sample1).expect("add to success");
        profile.add(sample2).expect("add to success");

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

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();
        let samples = serialized_profile.sorted_samples();

        let first = samples.get(0).expect("one sample");

        assert_eq!(first.values, vec![2, 10000, 42]);

        let second = samples.get(1).expect("one sample");

        assert_eq!(second.values, vec![5, 24, 99]);
    }

    #[test]
    fn test_upscaling_by_label_with_two_different_rules_on_two_different_sample() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

        let id_no_match_label = create_label("another label", Some("do not care"));

        let id_label = create_label("my label", Some("coco"));

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
            labels: vec![id_label, id_no_match_label],
        };

        let main_lines = vec![api::Line {
            function: api::Function {
                name: "{main}",
                system_name: "{main}",
                filename: "index.php",
                start_line: 0,
            },
            line: 0,
        }];

        let mapping = api::Mapping {
            filename: "php",
            ..Default::default()
        };

        let main_locations = vec![api::Location {
            mapping,
            lines: main_lines,
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

        profile.add(sample1).expect("add to success");
        profile.add(sample2).expect("add to success");

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

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();
        let samples = serialized_profile.sorted_samples();
        let first = samples.get(0).expect("one sample");

        assert_eq!(first.values, vec![2, 10000, 42]);

        let second = samples.get(1).expect("one sample");

        assert_eq!(second.values, vec![5, 24, 990]);
    }

    #[test]
    fn test_upscaling_by_label_on_two_values() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

        let id_label = create_label("my label", Some("coco"));

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
            labels: vec![id_label],
        };

        profile.add(sample1).expect("add to success");

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

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();

        let first = serialized_profile.samples.get(0).expect("one sample");

        assert_eq!(first.values, vec![2, 20000, 42]);
    }
    #[test]
    fn test_upscaling_by_value_and_by_label_different_values() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

        let id_label = create_label("my label", Some("coco"));

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
            labels: vec![id_label],
        };

        profile.add(sample1).expect("add to success");

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

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();

        let first = serialized_profile.samples.get(0).expect("one sample");

        assert_eq!(first.values, vec![2, 10000, 210]);
    }

    #[test]
    fn test_add_same_byvalue_rule_twice() {
        let sample_types = create_samples_types();

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

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

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

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

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

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

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

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

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

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

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

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

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

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

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

        let id_label = create_label("my label", Some("coco"));

        let sample1 = api::Sample {
            locations: vec![],
            values: vec![1, 10000, 42],
            labels: vec![id_label],
        };

        profile.add(sample1).expect("add to success");

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
    fn local_root_span_id_label_as_i64() {
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

        let mut profile: Profile = Profile::builder().sample_types(sample_types).build();

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

        profile.add(sample1).expect("add to success");
        profile.add(sample2).expect("add to success");

        profile.add_endpoint(10, Cow::from("endpoint 10"));
        profile.add_endpoint(large_span_id, Cow::from("large endpoint"));

        let serialized_profile = pprof::Profile::try_from(&profile).unwrap();
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
    }
}
