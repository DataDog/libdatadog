// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP profiles (`v1development`): an `ExportProfilesServiceRequest` with
//! one resource, one scope, one `Profile` per requested [`ProfileSpec`], and
//! a shared `ProfilesDictionary`.
//!
//! - The caller maps its sample types onto the sanctioned [`OtlpSampleType`]s; nothing else is
//!   emitted. Samples whose value for a type is 0 are left out of it.
//! - Trace context goes in the link table (trace id and span id). The local root span id and the
//!   trace endpoint have no semantic convention and are dropped.
//! - Only labels with a semantic-convention name become attributes; all other labels are dropped. A
//!   label qualifies if its key is one of the names every string table reserves (see [`semconv`]),
//!   matched by id, or a Datadog key with a known semantic-convention name (`thread id` becomes
//!   `thread.id`, ...), matched by text.
//! - What the Datadog intake reads from `event.json` goes into the resource (see [`Resource`]) and
//!   the scope instead.
//! - Datadog tag names with an OpenTelemetry equivalent are sent only under that name: the
//!   resource's known fields take their semantic-convention names, and so do tags with those
//!   Datadog names (see [`DATADOG_TAGS`]). Other tags pass through unchanged.
//!
//! [`semconv`]: crate::intern_table::semconv

use super::proto::Writer;
use super::{EncodeError, EncodeOptions, Numbering, Period, Resource, StringTable};
use crate::collections::alloc::Global;
use crate::collections::index_map::IndexMap;
use crate::collections::vec::TryVec;
use crate::intern_table::{FunctionId, StringId, semconv};
use crate::profile::{
    Frame, Function, LabelValue, Link, Profile, Resolve, SampleTypeId, Stacks, Str,
};

/// The sample types OTLP output may carry. Each has a fixed name and unit.
#[repr(C)]
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OtlpSampleType {
    CpuTime,
    AllocSize,
    AllocSamples,
    HeapLiveSize,
    HeapLiveSamples,
}

impl OtlpSampleType {
    /// The `ValueType` type written for it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::CpuTime => "cpu-time",
            Self::AllocSize => "alloc-size",
            Self::AllocSamples => "alloc-samples",
            Self::HeapLiveSize => "heap-live-size",
            Self::HeapLiveSamples => "heap-live-samples",
        }
    }

    /// The unit its source sample type must have.
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::CpuTime => "nanoseconds",
            Self::AllocSize | Self::HeapLiveSize => "bytes",
            Self::AllocSamples | Self::HeapLiveSamples => "count",
        }
    }
}

/// One OTLP `Profile` to write: the profile's `source` sample type, as the
/// sanctioned `sample_type`, with an optional period.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProfileSpec<'a> {
    pub source: SampleTypeId,
    pub sample_type: OtlpSampleType,
    pub period: Option<Period<'a>>,
}

/// The instrumentation scope: the profiler.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scope<'a> {
    pub name: &'a str,
    pub version: &'a str,
    pub attributes: &'a [(&'a str, &'a str)],
}

/// Datadog label keys that have semantic-convention names. This mapping may
/// become per-language, so these keys are matched by text rather than
/// reserved in the string table.
const SEMCONV_KEYS: &[(&str, &str)] = &[
    ("thread id", "thread.id"),
    ("thread name", "thread.name"),
    ("exception type", "exception.type"),
    ("exception message", "exception.message"),
];

fn attribute_key(key: &str) -> Option<&'static str> {
    SEMCONV_KEYS
        .iter()
        .find(|(datadog, _)| *datadog == key)
        .map(|&(_, semconv)| semconv)
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum AttrValue<'p> {
    Str(&'p str),
    Int(i64),
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct Attr<'p> {
    key: u64,
    value: AttrValue<'p>,
    unit: u64,
}

/// Encodes `profile`, which ends at `end_ns` (Unix nanoseconds).
///
/// # Errors
///
/// [`EncodeError::NothingToEncode`] if `specs` is empty, and
/// [`EncodeError::InvalidArgument`] if a spec's source is not one of the
/// profile's sample types or its unit is not the sanctioned type's, or if a
/// sanctioned type is requested twice; otherwise see [`EncodeError`].
#[allow(
    clippy::too_many_lines,
    reason = "one pass that mirrors the message layout"
)]
pub fn encode<S, F, K>(
    profile: &Profile<S, F, K>,
    end_ns: i64,
    resource: &Resource<'_>,
    scope: &Scope<'_>,
    specs: &[ProfileSpec<'_>],
    options: EncodeOptions,
) -> Result<TryVec<u8, Global>, EncodeError>
where
    S: Resolve<Str>,
    F: Resolve<Function>,
    K: Stacks,
{
    if specs.is_empty() {
        return Err(EncodeError::NothingToEncode);
    }
    let strings_source = profile.strings();
    let resolve = |id| strings_source.resolve(id).unwrap_or("");
    for (i, spec) in specs.iter().enumerate() {
        let source = profile
            .sample_type(spec.source)
            .ok_or(EncodeError::InvalidArgument)?;
        let repeated = specs
            .iter()
            .take(i)
            .any(|earlier| earlier.sample_type == spec.sample_type);
        if repeated || resolve(source.unit) != spec.sample_type.unit() {
            return Err(EncodeError::InvalidArgument);
        }
    }
    // The reserved semantic-convention ids, kept only where this profile's
    // strings agree, so a resolver that did not reserve them matches none.
    let mut reserved = [None::<(StringId, &str)>; semconv::IDS.len()];
    for (slot, &(id, name)) in reserved.iter_mut().zip(&semconv::IDS) {
        if strings_source.resolve(id) == Some(name) {
            *slot = Some((id, name));
        }
    }
    let attribute_name = |key: StringId| {
        reserved
            .iter()
            .flatten()
            .find(|(id, _)| *id == key)
            .map(|&(_, name)| name)
            .or_else(|| attribute_key(resolve(key)))
    };
    let mut tables = Tables {
        strings: StringTable::new()?,
        stacks: Numbering::new(),
        links: Numbering::new(),
        attributes: Numbering::new(),
        attribute_sets: Numbering::new(),
        label_attributes: IndexMap::new_in(Global),
    };
    // Index 0 of the link table is the empty link.
    tables.links.index(Link::default())?;
    let start = profile.start_ns();
    let mut w = Writer::new(options.max_bytes);
    let mut attribute_ids = TryVec::new_in(Global);
    let mut groups = Numbering::new();
    let mut scratch = Scratch {
        group_of: TryVec::new_in(Global),
        ends: TryVec::new_in(Global),
        values: TryVec::new_in(Global),
        timestamps: TryVec::new_in(Global),
    };

    w.message(1, |w| {
        w.message(1, |w| write_resource(w, resource))?;
        w.message(2, |w| {
            w.message(1, |w| write_scope(w, scope))?;
            for spec in specs {
                let Ok(type_index) = usize::try_from(spec.source.index()) else {
                    continue;
                };
                let name = tables.strings.index(spec.sample_type.name())?;
                let unit = tables.strings.index(spec.sample_type.unit())?;
                let period = match spec.period {
                    Some(p) => Some((
                        tables.strings.index(p.name)?,
                        tables.strings.index(p.unit)?,
                        p.value,
                    )),
                    None => None,
                };
                w.message(2, |w| {
                    w.message(1, |w| {
                        w.uint64(1, name)?;
                        w.uint64(2, unit)
                    })?;
                    group_samples(
                        profile,
                        type_index,
                        &mut tables,
                        &mut groups,
                        &mut scratch,
                        &mut attribute_ids,
                        &attribute_name,
                        &resolve,
                    )?;
                    let mut group_start = 0;
                    for (identity, &group_end) in groups.iter().zip(scratch.ends.iter()) {
                        let range = group_start..group_end;
                        group_start = group_end;
                        let attributes = usize::try_from(identity.attributes)
                            .ok()
                            .and_then(|set| tables.attribute_sets.get(set))
                            .map_or(&[][..], |set| set.as_slice());
                        let values = scratch.values.get(range.clone()).unwrap_or(&[]);
                        let timestamps = scratch.timestamps.get(range).unwrap_or(&[]);
                        w.message(2, |w| {
                            w.uint64(1, identity.stack)?;
                            w.packed_varints(2, attributes.iter().copied())?;
                            w.uint64(3, identity.link)?;
                            if identity.timed {
                                w.packed_varints(4, values.iter().map(|v| v.cast_unsigned()))?;
                                w.packed_fixed64(5, timestamps)?;
                            } else {
                                let total = values.iter().fold(0_i64, |a, &v| a.saturating_add(v));
                                w.packed_varints(4, core::iter::once(total.cast_unsigned()))?;
                            }
                            Ok(())
                        })?;
                    }
                    w.fixed64(3, start.cast_unsigned())?;
                    w.uint64(4, end_ns.saturating_sub(start).cast_unsigned())?;
                    if let Some((period_name, period_unit, value)) = period {
                        w.message(5, |w| {
                            w.uint64(1, period_name)?;
                            w.uint64(2, period_unit)
                        })?;
                        w.int64(6, value)?;
                    }
                    Ok(())
                })?;
            }
            Ok(())
        })
    })?;

    w.message(2, |w| write_dictionary(w, profile, &mut tables))?;
    Ok(w.finish())
}

struct Tables<'p> {
    strings: StringTable<'p>,
    stacks: Numbering<u32>,
    links: Numbering<Link>,
    attributes: Numbering<Attr<'p>>,
    /// Distinct attribute-index lists, as samples reference them.
    attribute_sets: Numbering<TryVec<u64, Global>>,
    /// Profile label set to its entry in `attribute_sets`. Label sets that
    /// differ only in dropped labels share an entry.
    label_attributes: IndexMap<u32, u64, Global>,
}

/// What OTLP combines samples by: `{stack_index, set_of(attribute_indices),
/// link_index}`. Timed and untimed samples are kept apart, since a `Sample`
/// has timestamps for all of its values or for none.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct Identity {
    stack: u64,
    attributes: u64,
    link: u64,
    timed: bool,
}

/// Per-sample-type buffers for grouping, reused across types.
struct Scratch {
    /// Each profile sample's group, or `SKIPPED`.
    group_of: TryVec<u32, Global>,
    /// Group `g` is `values[ends[g - 1]..ends[g]]` (from 0 for the first).
    ends: TryVec<usize, Global>,
    values: TryVec<i64, Global>,
    timestamps: TryVec<u64, Global>,
}

/// A sample left out of a type: its value for the type is 0.
const SKIPPED: u32 = u32::MAX;

/// Groups the samples with a nonzero value for `type_index` by their OTLP
/// identity, keeping profile order within each group: a stable counting
/// sort, so timed values stay in the order they were added.
#[allow(clippy::too_many_arguments, reason = "the encoder's shared state")]
fn group_samples<'p, S, F, K>(
    profile: &'p Profile<S, F, K>,
    type_index: usize,
    tables: &mut Tables<'p>,
    groups: &mut Numbering<Identity>,
    scratch: &mut Scratch,
    attribute_ids: &mut TryVec<u64, Global>,
    attribute_name: &impl Fn(StringId) -> Option<&'p str>,
    resolve: &impl Fn(StringId) -> &'p str,
) -> Result<(), EncodeError>
where
    S: Resolve<Str>,
    F: Resolve<Function>,
    K: Stacks,
{
    let oom = |_| EncodeError::OutOfMemory;
    *groups = Numbering::new();
    scratch.group_of.clear();
    // Counts per group first; turned into end offsets below.
    scratch.ends.clear();
    for (key, values, timestamp) in profile.samples() {
        if values.get(type_index).copied().unwrap_or(0) == 0 {
            scratch.group_of.try_push(SKIPPED).map_err(oom)?;
            continue;
        }
        let attributes = if let Some(&set) = tables.label_attributes.get(&key.labels) {
            set
        } else {
            attribute_ids.clear();
            for label in profile.labels(key.labels) {
                let Some(name) = attribute_name(label.key) else {
                    continue;
                };
                let attr = Attr {
                    key: tables.strings.index(name)?,
                    value: match label.value {
                        LabelValue::Str(s) => AttrValue::Str(resolve(s)),
                        LabelValue::Local(s) => AttrValue::Str(profile.local_string(s)),
                        LabelValue::Num { value, .. } => AttrValue::Int(value),
                    },
                    unit: match label.value {
                        LabelValue::Num { unit, .. } => tables.strings.index(resolve(unit))?,
                        LabelValue::Str(_) | LabelValue::Local(_) => 0,
                    },
                };
                let id = tables.attributes.index(attr)? + 1;
                attribute_ids.try_push(id).map_err(oom)?;
            }
            let set = tables
                .attribute_sets
                .index(attribute_ids.try_clone().map_err(oom)?)?;
            tables
                .label_attributes
                .try_insert(key.labels, set)
                .map_err(oom)?;
            set
        };
        let identity = Identity {
            stack: tables.stacks.index(key.stack)? + 1,
            attributes,
            link: tables.links.index(profile.link_at(key.link))?,
            timed: timestamp != 0,
        };
        let group = groups.index(identity)?;
        let group = u32::try_from(group)
            .ok()
            .filter(|&g| g != SKIPPED)
            .ok_or(EncodeError::TooLarge)?;
        scratch.group_of.try_push(group).map_err(oom)?;
        match usize::try_from(group)
            .ok()
            .and_then(|g| scratch.ends.get_mut(g))
        {
            Some(count) => *count += 1,
            None => scratch.ends.try_push(1).map_err(oom)?,
        }
    }

    // Counts to start offsets; the scatter below advances each to its end.
    let mut total = 0_usize;
    for slot in scratch.ends.iter_mut() {
        let count = *slot;
        *slot = total;
        total += count;
    }
    scratch.values.clear();
    scratch.values.try_resize(total, &0).map_err(oom)?;
    scratch.timestamps.clear();
    scratch.timestamps.try_resize(total, &0).map_err(oom)?;
    for ((_, values, timestamp), &group) in profile.samples().zip(scratch.group_of.iter()) {
        let Some(next) = usize::try_from(group)
            .ok()
            .and_then(|g| scratch.ends.get_mut(g))
        else {
            continue;
        };
        let at = *next;
        *next += 1;
        if let (Some(value), Some(slot), Some(ts)) = (
            values.get(type_index),
            scratch.values.get_mut(at),
            scratch.timestamps.get_mut(at),
        ) {
            *slot = *value;
            *ts = timestamp.cast_unsigned();
        }
    }
    Ok(())
}

/// Datadog tag names and their OpenTelemetry names. The resource's known
/// fields ([`Resource::fields`]) are included too, so a tag can never send a
/// field's Datadog name. Tags not listed are resource attributes as-is.
const DATADOG_TAGS: &[(&str, &str)] = &[
    ("process_id", "process.pid"),
    ("git.repository_url", "vcs.repository.url.full"),
    ("git.commit.sha", "vcs.ref.head.revision"),
    ("functionname", "faas.name"),
    ("container_name", "container.name"),
    ("ecs_container_name", "container.name"),
    ("image_name", "container.image.name"),
    ("image_tag", "container.image.tags"),
    ("kube_namespace", "k8s.namespace.name"),
    ("pod_name", "k8s.pod.name"),
    ("kube_container_name", "k8s.container.name"),
    ("kube_cluster_name", "k8s.cluster.name"),
    ("kube_deployment", "k8s.deployment.name"),
    ("kube_replica_set", "k8s.replicaset.name"),
    ("kube_stateful_set", "k8s.statefulset.name"),
    ("kube_daemon_set", "k8s.daemonset.name"),
    ("kube_job", "k8s.job.name"),
    ("kube_cronjob", "k8s.cronjob.name"),
    ("kube_node", "k8s.node.name"),
    ("cloud_provider", "cloud.provider"),
    ("region", "cloud.region"),
    ("zone", "cloud.availability_zone"),
    ("task_arn", "aws.ecs.task.arn"),
    ("task_family", "aws.ecs.task.family"),
    ("task_version", "aws.ecs.task.revision"),
];

/// The OTLP resource attribute name of the tag named `key`.
fn tag_key(key: &str) -> &str {
    if let Some(&(_, semconv, _)) = Resource::default()
        .fields()
        .iter()
        .find(|(datadog, _, _)| *datadog == key)
    {
        return semconv;
    }
    DATADOG_TAGS
        .iter()
        .find(|(datadog, _)| *datadog == key)
        .map_or(key, |&(_, semconv)| semconv)
}

/// The resource attributes written so far, so a key is sent at most once:
/// the first value wins, and the known fields are written first.
struct Written<'a>(TryVec<&'a str, Global>);

impl<'a> Written<'a> {
    fn with_capacity(capacity: usize) -> Result<Self, EncodeError> {
        TryVec::try_with_capacity_in(capacity, Global)
            .map(Self)
            .map_err(|_| EncodeError::OutOfMemory)
    }

    /// Records `key`; false if it was already written.
    fn first(&mut self, key: &'a str) -> Result<bool, EncodeError> {
        if self.0.contains(&key) {
            return Ok(false);
        }
        self.0.try_push(key).map_err(|_| EncodeError::OutOfMemory)?;
        Ok(true)
    }
}

fn write_resource(w: &mut Writer, resource: &Resource<'_>) -> Result<(), EncodeError> {
    let fields = resource.fields();
    let mut written = Written::with_capacity(fields.len() + 2 + resource.tags.len())?;
    for (_, key, value) in fields {
        if !value.is_empty() && written.first(key)? {
            key_value(w, 1, key, value)?;
        }
    }
    if resource.process_id != 0 && written.first("process.pid")? {
        w.message(1, |w| {
            w.string(1, "process.pid")?;
            w.message(2, |w| w.int64(3, i64::from(resource.process_id)))
        })?;
    }
    if !resource.runtime_description.is_empty() && written.first("process.runtime.description")? {
        key_value(
            w,
            1,
            "process.runtime.description",
            resource.runtime_description,
        )?;
    }
    if let Ok(inode) = i64::try_from(resource.cgroup_inode)
        && inode != 0
        && written.first("datadog.container.cgroup_inode")?
    {
        w.message(1, |w| {
            w.string(1, "datadog.container.cgroup_inode")?;
            w.message(2, |w| w.int64(3, inode))
        })?;
    }
    if written.first("telemetry.sdk.name")? {
        key_value(w, 1, "telemetry.sdk.name", "datadog")?;
    }
    for &(key, value) in resource.tags {
        let key = tag_key(key);
        if written.first(key)? {
            key_value(w, 1, key, value)?;
        }
    }
    Ok(())
}

/// The instrumentation scope: the profiler, with the caller's attributes.
fn write_scope(w: &mut Writer, scope: &Scope<'_>) -> Result<(), EncodeError> {
    w.string(1, scope.name)?;
    w.string(2, scope.version)?;
    let mut written = Written::with_capacity(scope.attributes.len())?;
    for &(key, value) in scope.attributes {
        if written.first(key)? {
            key_value(w, 3, key, value)?;
        }
    }
    Ok(())
}

/// A `KeyValue` with a string value, as field `field`.
fn key_value(w: &mut Writer, field: u32, key: &str, value: &str) -> Result<(), EncodeError> {
    w.message(field, |w| {
        w.string(1, key)?;
        w.message(2, |w| w.string(1, value))
    })
}

fn write_dictionary<'p, S, F, K>(
    w: &mut Writer,
    profile: &'p Profile<S, F, K>,
    tables: &mut Tables<'p>,
) -> Result<(), EncodeError>
where
    S: Resolve<Str>,
    F: Resolve<Function>,
    K: Stacks,
{
    let strings_source = profile.strings();
    let resolve = |id| strings_source.resolve(id).unwrap_or("");
    let mut locations = Numbering::<Frame>::new();
    let mut functions = Numbering::<FunctionId>::new();

    // mapping_table: only the zero entry.
    w.message(1, |_| Ok(()))?;

    // stack_table, which numbers the locations it uses.
    w.message(7, |_| Ok(()))?;
    let mut ids = TryVec::new_in(Global);
    for &key in tables.stacks.iter() {
        ids.clear();
        for frame in profile.stacks().frames(key).unwrap_or(&[]) {
            let id = locations.index(*frame)? + 1;
            ids.try_push(id).map_err(|_| EncodeError::OutOfMemory)?;
        }
        w.message(7, |w| w.packed_varints(1, ids.iter().copied()))?;
    }

    // location_table, which numbers the functions it uses.
    w.message(2, |_| Ok(()))?;
    for frame in locations.iter() {
        let function = functions.index(frame.function)? + 1;
        w.message(2, |w| {
            w.uint64(2, frame.address)?;
            w.message(3, |w| {
                w.uint64(1, function)?;
                w.int64(2, frame.line)
            })
        })?;
    }

    // function_table, which adds its strings.
    w.message(3, |_| Ok(()))?;
    let functions_source = profile.functions();
    for &id in functions.iter() {
        let function = functions_source.resolve(id).unwrap_or_default();
        let name = tables.strings.index(resolve(function.name))?;
        let system_name = tables.strings.index(resolve(function.system_name))?;
        let filename = tables.strings.index(resolve(function.filename))?;
        w.message(3, |w| {
            w.uint64(1, name)?;
            w.uint64(2, system_name)?;
            w.uint64(3, filename)?;
            w.int64(4, i64::from(function.start_line))
        })?;
    }

    // link_table; index 0 is the empty link.
    for link in tables.links.iter() {
        w.message(4, |w| {
            if !link.is_empty() {
                w.bytes(1, &link.trace_id)?;
                w.bytes(2, &link.span_id)?;
            }
            Ok(())
        })?;
    }

    // attribute_table.
    w.message(6, |_| Ok(()))?;
    for attr in tables.attributes.iter() {
        w.message(6, |w| {
            w.uint64(1, attr.key)?;
            w.message(2, |w| match attr.value {
                AttrValue::Str(s) => w.string(1, s),
                AttrValue::Int(i) => w.int64(3, i),
            })?;
            w.uint64(3, attr.unit)
        })?;
    }

    // string_table, last: everything above may have added strings.
    for text in tables.strings.iter() {
        w.bytes(5, text.as_bytes())?;
    }
    Ok(())
}
