// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use bolero::generator::TypeGenerator;
use core::cmp::Ordering;
use core::hash::Hasher;
use core::ops::Deref;
use std::collections::HashSet;

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, TypeGenerator)]
pub struct Function {
    /// Name of the function, in human-readable form if available.
    pub name: Box<str>,

    /// Name of the function, as identified by the system.
    /// For instance, it can be a C++ mangled name.
    pub system_name: Box<str>,

    /// Source file containing the function.
    pub filename: Box<str>,
}

impl Function {
    pub fn new(name: Box<str>, system_name: Box<str>, filename: Box<str>) -> Self {
        Self {
            name,
            system_name,
            filename,
        }
    }
}

impl<'a> From<&'a Function> for api::Function<'a> {
    fn from(value: &'a Function) -> Self {
        Self {
            name: &value.name,
            system_name: &value.system_name,
            filename: &value.filename,
        }
    }
}

#[derive(Clone, Debug, TypeGenerator)]
pub enum LabelValue {
    Str(Box<str>),
    Num { num: i64, num_unit: Box<str> },
}

impl Default for LabelValue {
    fn default() -> Self {
        LabelValue::Str(Box::from(""))
    }
}

#[derive(Clone, Debug, Default, TypeGenerator)]
pub struct Label {
    pub key: Box<str>,
    pub value: LabelValue,
}

impl From<(Box<str>, LabelValue)> for Label {
    fn from((key, value): (Box<str>, LabelValue)) -> Self {
        Label { key, value }
    }
}

impl From<(&Box<str>, &LabelValue)> for Label {
    fn from((key, value): (&Box<str>, &LabelValue)) -> Self {
        Label::from((key.clone(), value.clone()))
    }
}

impl From<&(Box<str>, LabelValue)> for Label {
    fn from(tuple: &(Box<str>, LabelValue)) -> Self {
        Label::from(tuple.clone())
    }
}

impl<'a> From<&'a Label> for api::Label<'a> {
    fn from(label: &'a Label) -> Self {
        let (str, num, num_unit) = match &label.value {
            LabelValue::Str(str) => (str.deref(), 0, ""),
            LabelValue::Num { num, num_unit } => ("", *num, num_unit.deref()),
        };
        Self {
            key: &label.key,
            str,
            num,
            num_unit,
        }
    }
}

impl PartialEq for Label {
    fn eq(&self, other: &Self) -> bool {
        api::Label::from(self).eq(&api::Label::from(other))
    }
}

impl PartialOrd for Label {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for Label {}

impl Ord for Label {
    fn cmp(&self, other: &Self) -> Ordering {
        api::Label::from(self).cmp(&api::Label::from(other))
    }
}

impl core::hash::Hash for Label {
    fn hash<H: Hasher>(&self, state: &mut H) {
        api::Label::from(self).hash(state);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, TypeGenerator)]
pub struct Line {
    /// The corresponding profile.Function for this line.
    pub function: Function,

    /// Line number in source code.
    pub line: i64,
}

impl<'a> From<&'a Line> for api::Line<'a> {
    fn from(value: &'a Line) -> Self {
        Self {
            function: (&value.function).into(),
            line: value.line,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, TypeGenerator)]
pub struct Location {
    pub mapping: Mapping,
    pub function: Function,

    /// The instruction address for this location, if available.  It
    /// should be within [Mapping.memory_start...Mapping.memory_limit]
    /// for the corresponding mapping. A non-leaf address may be in the
    /// middle of a call instruction. It is up to display tools to find
    /// the beginning of the instruction if necessary.
    pub address: u64,
    pub line: i64,
}

impl Location {
    pub fn new(mapping: Mapping, function: Function, address: u64, line: i64) -> Self {
        Self {
            mapping,
            function,
            address,
            line,
        }
    }
}

impl<'a> From<&'a Location> for api::Location<'a> {
    fn from(value: &'a Location) -> Self {
        Self {
            mapping: (&value.mapping).into(),
            function: (&value.function).into(),
            address: value.address,
            line: value.line,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, TypeGenerator)]
pub struct Mapping {
    /// Address at which the binary (or DLL) is loaded into memory.
    pub memory_start: u64,

    /// The limit of the address range occupied by this mapping.
    pub memory_limit: u64,

    /// Offset in the binary that corresponds to the first mapped address.
    pub file_offset: u64,

    /// The object this entry is loaded from.  This can be a filename on
    /// disk for the main binary and shared libraries, or virtual
    /// abstractions like "[vdso]".
    pub filename: Box<str>,

    /// A string that uniquely identifies a particular program version
    /// with high probability. E.g., for binaries generated by GNU tools,
    /// it could be the contents of the .note.gnu.build-id field.
    pub build_id: Box<str>,
}

impl Mapping {
    pub fn new(
        memory_start: u64,
        memory_limit: u64,
        file_offset: u64,
        filename: Box<str>,
        build_id: Box<str>,
    ) -> Self {
        Self {
            memory_start,
            memory_limit,
            file_offset,
            filename,
            build_id,
        }
    }
}

impl<'a> From<&'a Mapping> for api::Mapping<'a> {
    fn from(value: &'a Mapping) -> Self {
        Self {
            memory_start: value.memory_start,
            memory_limit: value.memory_limit,
            file_offset: value.file_offset,
            filename: &value.filename,
            build_id: &value.build_id,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Sample {
    /// The leaf is at locations[0].
    pub locations: Vec<Location>,

    /// The type and unit of each value is defined by the corresponding
    /// entry in Profile.sample_type. All samples must have the same
    /// number of values, the same as the length of Profile.sample_type.
    /// When aggregating multiple samples into a single sample, the
    /// result has a list of values that is the element-wise sum of the
    /// lists of the originals.
    pub values: Vec<i64>,

    /// label includes additional context for this sample. It can include
    /// things like a thread id, allocation size, etc
    pub labels: Vec<Label>,
}

/// Since [Sample] needs a Vec<Label> which have unique keys, we generate
/// samples using this parallel struct, and then map it to the [Sample].
#[derive(Clone, Debug, TypeGenerator)]
struct FuzzSample {
    pub locations: Vec<Location>,
    pub values: Vec<i64>,
    pub labels: HashMap<Box<str>, LabelValue>,
}

impl From<FuzzSample> for Sample {
    fn from(sample: FuzzSample) -> Self {
        Self {
            locations: sample.locations,
            values: sample.values,
            labels: sample.labels.into_iter().map(Label::from).collect(),
        }
    }
}

impl From<&FuzzSample> for Sample {
    fn from(sample: &FuzzSample) -> Self {
        Self {
            locations: sample.locations.clone(),
            values: sample.values.clone(),
            labels: sample.labels.iter().map(Label::from).collect(),
        }
    }
}

#[cfg(test)]
impl Sample {
    /// Checks if the sample is well-formed.  Useful in testing.
    pub fn is_well_formed(&self) -> bool {
        let labels_are_unique = {
            let mut uniq = HashSet::new();
            self.labels.iter().map(|l| &l.key).all(|x| uniq.insert(x))
        };
        labels_are_unique
    }
}

impl<'a> From<&'a Sample> for api::Sample<'a> {
    fn from(value: &'a Sample) -> Self {
        Self {
            locations: value.locations.iter().map(api::Location::from).collect(),
            values: &value.values,
            labels: value.labels.iter().map(api::Label::from).collect(),
        }
    }
}

#[track_caller]
fn assert_sample_types_eq(
    profile: &pprof::Profile,
    expected_sample_types: &[owned_types::ValueType],
) {
    assert_eq!(
        profile.sample_types.len(),
        expected_sample_types.len(),
        "Sample types length mismatch"
    );
    for (typ, expected_typ) in profile
        .sample_types
        .iter()
        .zip(expected_sample_types.iter())
    {
        assert_eq!(*profile.string_table_fetch(typ.r#type), *expected_typ.r#typ);
        assert_eq!(*profile.string_table_fetch(typ.unit), *expected_typ.unit);
    }
}

#[track_caller]
fn assert_samples_eq(
    original_samples: &[(Option<Timestamp>, Sample)],
    profile: &pprof::Profile,
    samples_with_timestamps: &[&Sample],
    samples_without_timestamps: &HashMap<(&[Location], &[Label]), Vec<i64>>,
    endpoint_mappings: &FxIndexMap<u64, &String>,
) {
    assert_eq!(
        profile.samples.len(),
        samples_with_timestamps.len() + samples_without_timestamps.len(),
        "Samples length mismatch: {original_samples:#?}"
    );

    let mut expected_timestamped_samples = samples_with_timestamps.iter();

    for sample in profile.samples.iter() {
        // Recreate owned_locations from vector of pprof::Location
        let mut owned_locations = Vec::new();
        for loc_id in sample.location_ids.iter() {
            // Subtract 1 because when creating pprof location ids, we use
            // `small_non_zero_pprof_id()` function which guarantees that the id stored in pprof
            // is +1 of the index in the vector of Locations in internal::Profile.
            let location = &profile.locations[*loc_id as usize - 1];

            // PHP, Python, and Ruby don't use mappings, so allow for zero id.
            let mapping = if location.mapping_id != 0 {
                profile.mappings[location.mapping_id as usize - 1]
            } else {
                Default::default()
            };
            // internal::Location::to_pprof() always creates a single line.
            assert_eq!(1, location.lines.len());
            let line = location.lines[0];
            let function = profile.functions[line.function_id as usize - 1];
            assert!(!location.is_folded);

            // TODO: Consider using &str from the string table and make an `api::` mapping
            // to save allocations.
            let owned_mapping = Mapping::new(
                mapping.memory_start,
                mapping.memory_limit,
                mapping.file_offset,
                profile.string_table_fetch_owned(mapping.filename),
                profile.string_table_fetch_owned(mapping.build_id),
            );
            let owned_function = Function::new(
                profile
                    .string_table_fetch(function.name)
                    .clone()
                    .into_boxed_str(),
                profile.string_table_fetch_owned(function.system_name),
                profile.string_table_fetch_owned(function.filename),
            );
            let owned_location =
                Location::new(owned_mapping, owned_function, location.address, line.line);

            owned_locations.push(owned_location);
        }

        // Recreate owned_labels from vector of pprof::Label
        let mut owned_labels = Vec::new();
        for label in sample.labels.iter() {
            let key = profile.string_table_fetch_owned(label.key);

            if *key == *"end_timestamp_ns" {
                // TODO: Check end timestamp label
                continue;
            } else if *key == *"trace endpoint" {
                let actual_str = profile.string_table_fetch(label.str);
                let prev_label: &Label = owned_labels
                    .last()
                    .expect("Previous label to exist for endpoint label");

                let num = match &prev_label.value {
                    LabelValue::Str(str) => {
                        panic!("Unexpected label value of type str for trace endpoint: {str}")
                    }
                    LabelValue::Num { num, .. } => *num as u64,
                };
                let expected_str = endpoint_mappings
                    .get(&num)
                    .expect("Endpoint mapping to exist");
                assert_eq!(actual_str, *expected_str);
                continue;
            }

            if label.str != 0 {
                let str = Box::from(profile.string_table_fetch(label.str).as_str());
                owned_labels.push(Label {
                    key,
                    value: LabelValue::Str(str),
                });
            } else {
                let num = label.num;
                let num_unit = profile.string_table_fetch_owned(label.num_unit);
                owned_labels.push(Label {
                    key,
                    value: LabelValue::Num { num, num_unit },
                });
            }
        }

        if let Some(expected_sample) = expected_timestamped_samples.next() {
            assert_eq!(owned_locations, expected_sample.locations);
            assert_eq!(sample.values, expected_sample.values);
            assert_eq!(owned_labels, expected_sample.labels);
        } else {
            let key: (&[Location], &[Label]) = (&owned_locations, &owned_labels);
            let Some(expected_values) = samples_without_timestamps.get(&key) else {
                panic!("Value not found for an aggregated sample key {key:#?} in {original_samples:#?}")
            };
            assert_eq!(&sample.values, expected_values);
        }
    }
}

fn fuzz_add_sample<'a>(
    timestamp: &Option<Timestamp>,
    sample: &'a Sample,
    expected_sample_types: &[owned_types::ValueType],
    profile: &mut Profile,
    samples_with_timestamps: &mut Vec<&'a Sample>,
    samples_without_timestamps: &mut HashMap<(&'a [Location], &'a [Label]), Vec<i64>>,
) {
    let r = profile.add_sample(sample.into(), *timestamp);
    if expected_sample_types.len() == sample.values.len() {
        assert!(r.is_ok());
        if timestamp.is_some() {
            samples_with_timestamps.push(sample);
        } else if let Some(existing_values) =
            samples_without_timestamps.get_mut(&(&sample.locations, &sample.labels))
        {
            existing_values
                .iter_mut()
                .zip(sample.values.iter())
                .for_each(|(a, b)| *a = a.saturating_add(*b));
        } else {
            samples_without_timestamps
                .insert((&sample.locations, &sample.labels), sample.values.clone());
        }
    } else {
        assert!(r.is_err());
    }
}

#[test]
fn fuzz_failure_001() {
    let sample_types = [];
    let expected_sample_types = &[];
    let original_samples = vec![(
        None,
        Sample {
            locations: vec![],
            values: vec![],
            labels: vec![
                Label {
                    key: Box::from(""),
                    value: LabelValue::Str(Box::from("")),
                },
                Label {
                    key: Box::from("local root span id"),
                    value: LabelValue::Num {
                        num: 281474976710656,
                        num_unit: Box::from(""),
                    },
                },
            ],
        },
    )];
    let mut expected_profile = Profile::new(&sample_types, None);
    let mut samples_with_timestamps = Vec::new();
    let mut samples_without_timestamps: HashMap<(&[Location], &[Label]), Vec<i64>> = HashMap::new();

    fuzz_add_sample(
        &original_samples[0].0,
        &original_samples[0].1,
        expected_sample_types,
        &mut expected_profile,
        &mut samples_with_timestamps,
        &mut samples_without_timestamps,
    );

    let profile = pprof::roundtrip_to_pprof(expected_profile).unwrap();
    assert_sample_types_eq(&profile, expected_sample_types);
    assert_samples_eq(
        &original_samples,
        &profile,
        &samples_with_timestamps,
        &samples_without_timestamps,
        &FxIndexMap::default(),
    );
}

/// Fuzzes adding a bunch of samples to the profile.
#[test]
#[cfg_attr(miri, ignore)]
fn test_fuzz_add_sample() {
    let sample_types_gen = Vec::<owned_types::ValueType>::produce();
    let samples_gen = Vec::<(Option<Timestamp>, FuzzSample)>::produce();

    bolero::check!()
        .with_generator((sample_types_gen, samples_gen))
        .for_each(|(expected_sample_types, samples)| {
            let samples = samples
                .iter()
                .map(|(tstamp, sample)| (*tstamp, Sample::from(sample)))
                .collect::<Vec<_>>();

            let sample_types: Vec<_> = expected_sample_types
                .iter()
                .map(api::ValueType::from)
                .collect();
            let mut expected_profile = Profile::new(&sample_types, None);
            let mut samples_with_timestamps = Vec::new();
            let mut samples_without_timestamps: HashMap<(&[Location], &[Label]), Vec<i64>> =
                HashMap::new();
            for (timestamp, sample) in &samples {
                fuzz_add_sample(
                    timestamp,
                    sample,
                    expected_sample_types,
                    &mut expected_profile,
                    &mut samples_with_timestamps,
                    &mut samples_without_timestamps,
                );
            }
            let profile = pprof::roundtrip_to_pprof(expected_profile).unwrap();
            assert_sample_types_eq(&profile, expected_sample_types);
            assert_samples_eq(
                &samples,
                &profile,
                &samples_with_timestamps,
                &samples_without_timestamps,
                &FxIndexMap::default(),
            );
        })
}

#[test]
#[cfg_attr(miri, ignore)]
fn fuzz_add_sample_with_fixed_sample_length() {
    let sample_length_gen = 1..=64usize;

    bolero::check!()
        .with_shrink_time(Duration::from_secs(60))
        .with_generator(sample_length_gen)
        .and_then(|sample_len| {
            let sample_types = Vec::<owned_types::ValueType>::produce()
                .with()
                .len(sample_len);

            let timestamps = Option::<Timestamp>::produce();
            let locations = Vec::<Location>::produce();
            let values = Vec::<i64>::produce().with().len(sample_len);
            // Generate labels with unique keys
            let labels = HashMap::<Box<str>, LabelValue>::produce();

            let samples = Vec::<(
                Option<Timestamp>,
                Vec<Location>,
                Vec<i64>,
                HashMap<Box<str>, LabelValue>,
            )>::produce()
            .with()
            .values((timestamps, locations, values, labels));
            (sample_types, samples)
        })
        .for_each(|(sample_types, samples)| {
            let api_sample_types: Vec<_> = sample_types.iter().map(api::ValueType::from).collect();
            let mut profile = Profile::new(&api_sample_types, None);
            let mut samples_with_timestamps = Vec::new();
            let mut samples_without_timestamps: HashMap<(&[Location], &[Label]), Vec<i64>> =
                HashMap::new();

            let samples: Vec<(Option<Timestamp>, Sample)> = samples
                .iter()
                .map(|(timestamp, locations, values, labels)| {
                    (
                        *timestamp,
                        Sample {
                            locations: locations.clone(),
                            values: values.clone(),
                            labels: labels.iter().map(Label::from).collect::<Vec<Label>>(),
                        },
                    )
                })
                .collect();

            for (timestamp, sample) in samples.iter() {
                fuzz_add_sample(
                    timestamp,
                    sample,
                    sample_types,
                    &mut profile,
                    &mut samples_with_timestamps,
                    &mut samples_without_timestamps,
                );
            }
            let serialized_profile =
                pprof::roundtrip_to_pprof(profile).expect("Failed to roundtrip to pprof");

            assert_sample_types_eq(&serialized_profile, sample_types);
            assert_samples_eq(
                &samples,
                &serialized_profile,
                &samples_with_timestamps,
                &samples_without_timestamps,
                &FxIndexMap::default(),
            );
        });
}

#[test]
fn fuzz_add_endpoint() {
    bolero::check!()
        .with_type::<Vec<(u64, String)>>()
        .for_each(|endpoints| {
            let mut profile = Profile::new(&[], None);
            for (local_root_span_id, endpoint) in endpoints {
                profile
                    .add_endpoint(*local_root_span_id, endpoint.into())
                    .expect("add_endpoint to succeed");
            }
            pprof::roundtrip_to_pprof(profile).expect("roundtrip_to_pprof to succeed");
        });
}

#[test]
fn fuzz_add_endpoint_count() {
    bolero::check!()
        .with_type::<Vec<(String, i64)>>()
        .for_each(|endpoint_counts| {
            let mut profile = Profile::new(&[], None);
            for (endpoint, count) in endpoint_counts {
                profile
                    .add_endpoint_count(endpoint.into(), *count)
                    .expect("add_endpoint_count to succeed");
            }
            pprof::roundtrip_to_pprof(profile).expect("roundtrip_to_pprof to succeed");
        });
}

#[derive(Debug, TypeGenerator)]
enum FuzzOperation {
    AddSample(Option<Timestamp>, FuzzSample),
    AddEndpoint(u64, String),
}

#[derive(Debug)]
enum Operation {
    AddSample(Option<Timestamp>, Sample),
    AddEndpoint(u64, String),
}

impl From<&FuzzOperation> for Operation {
    fn from(operation: &FuzzOperation) -> Self {
        match operation {
            FuzzOperation::AddSample(tstamp, sample) => {
                Operation::AddSample(*tstamp, Sample::from(sample))
            }
            FuzzOperation::AddEndpoint(id, endpoint) => {
                Operation::AddEndpoint(*id, endpoint.clone())
            }
        }
    }
}

#[derive(Debug, TypeGenerator)]
struct ApiFunctionCalls {
    sample_types: Vec<owned_types::ValueType>,
    operations: Vec<FuzzOperation>,
}

#[test]
#[cfg_attr(miri, ignore)]
fn fuzz_api_function_calls() {
    let sample_length_gen = 1..=64usize;

    bolero::check!()
        .with_generator(sample_length_gen)
        .and_then(|sample_len| {
            let sample_types = Vec::<owned_types::ValueType>::produce()
                .with()
                .len(sample_len);
            let operations = Vec::<FuzzOperation>::produce();

            (sample_types, operations)
        })
        .for_each(|(sample_types, operations)| {
            let operations = operations.iter().map(Operation::from).collect::<Vec<_>>();

            let api_sample_types: Vec<_> = sample_types.iter().map(api::ValueType::from).collect();
            let mut profile = Profile::new(&api_sample_types, None);
            let mut samples_with_timestamps: Vec<&Sample> = Vec::new();
            let mut samples_without_timestamps: HashMap<(&[Location], &[Label]), Vec<i64>> =
                HashMap::new();
            let mut endpoint_mappings: FxIndexMap<u64, &String> = FxIndexMap::default();

            let mut original_samples = Vec::new();

            for operation in &operations {
                match operation {
                    Operation::AddSample(timestamp, sample) => {
                        // Track the inputs for debugging.
                        original_samples.push((*timestamp, sample.clone()));

                        fuzz_add_sample(
                            timestamp,
                            sample,
                            sample_types,
                            &mut profile,
                            &mut samples_with_timestamps,
                            &mut samples_without_timestamps,
                        );
                    }
                    Operation::AddEndpoint(local_root_span_id, endpoint) => {
                        profile
                            .add_endpoint(*local_root_span_id, endpoint.into())
                            .expect("add_endpoint to succeed");
                        endpoint_mappings.insert(*local_root_span_id, endpoint);
                    }
                }
            }

            let pprof_profile = pprof::roundtrip_to_pprof(profile).unwrap();
            assert_sample_types_eq(&pprof_profile, sample_types);
            assert_samples_eq(
                &original_samples,
                &pprof_profile,
                &samples_with_timestamps,
                &samples_without_timestamps,
                &endpoint_mappings,
            );
        })
}
