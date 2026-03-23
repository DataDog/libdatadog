// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[cfg(feature = "otel")]
use crate::api::SampleType;
use crate::api::UpscalingInfo;
use anyhow::Context;
#[cfg(feature = "otel")]
use enum_map::EnumMap;

#[derive(Debug)]
pub struct UpscalingRule {
    upscaling_info: UpscalingInfo,
    values_offset: Vec<usize>,
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
            UpscalingInfo::PoissonNonSampleTypeCount {
                sum_value_offset,
                count_value,
                sampling_distance,
            } => {
                // This should not happen, but if it happens,
                // do not upscale
                if values[sum_value_offset] == 0 || count_value == 0 {
                    return 1_f64;
                }

                let avg = values[sum_value_offset] as f64 / count_value as f64;
                1_f64 / (1_f64 - (-avg / sampling_distance as f64).exp())
            }
            UpscalingInfo::Proportional { scale } => scale,
        }
    }

    pub fn new(values_offset: Vec<usize>, upscaling_info: UpscalingInfo) -> Self {
        Self {
            values_offset,
            upscaling_info,
        }
    }
}

/// The active upscaling rules type for this build.
///
/// When the `otel` feature is enabled this is [OtelUpscalingRules], which stores rules indexed
/// by [SampleType] and applies them one value at a time. Otherwise it is the pprof-style
/// [UpscalingRules], which operates on a full value vector.
#[cfg(not(feature = "otel"))]
pub type ActiveUpscalingRules = UpscalingRules;
#[cfg(feature = "otel")]
pub type ActiveUpscalingRules = OtelUpscalingRules;

#[derive(Default)]
pub struct UpscalingRules {
    rules: FxIndexMap<(StringId, StringId), Vec<UpscalingRule>>,
    // this is just an optimization in the case where we check collisions (when adding
    // a by-value rule) against by-label rules
    // 32 should be enough for the size of the bitmap
    offset_modified_by_bylabel_rule: bitmaps::Bitmap<32>,
}

impl UpscalingRules {
    pub fn add(
        &mut self,
        values_offset: &[usize],
        label_name: (&str, StringId),
        label_value: (&str, StringId),
        upscaling_info: UpscalingInfo,
        max_offset: usize,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            values_offset.iter().all(|x| *x < max_offset),
            "Invalid offset. Highest expected offset: {max_offset}",
        );

        let mut new_values_offset = values_offset.to_vec();
        new_values_offset.sort_unstable();

        self.check_collisions(&new_values_offset, label_name, label_value, &upscaling_info)?;
        upscaling_info.check_validity(max_offset)?;
        let rule: UpscalingRule = UpscalingRule::new(new_values_offset, upscaling_info);

        let label_name_id = label_name.1;
        let label_value_id = label_value.1;
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
                let (_, rules) = self.rules.get_index_mut(index).with_context(|| {
                    format!("Expected upscaling rules to exist for index {index}")
                })?;
                rules.push(rule);
            }
        };
        Ok(())
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
        let (label_name_str, label_name_id) = label_name;
        let (label_value_str, label_value_id) = label_value;

        let colliding_rule = match self.rules.get(&(label_name_id, label_value_id)) {
            Some(rules) => rules
                .iter()
                .find(|rule| is_overlapping(&rule.values_offset, values_offset)),
            None => None,
        };

        anyhow::ensure!(
            colliding_rule.is_none(),
            "There are duplicated by-label rules for the same label name: {label_name_str} with at least one value offset in common.\n\
            Existing rule {colliding_rule:?}\n\
            New rule {label_name_str} {label_value_str} {values_offset:?} {upscaling_info:?}"
        );

        // if we are adding a by-value rule, we need to check against
        // all by-label rules for collisions
        if label_name.1.is_zero() && label_value.1.is_zero() {
            let collision_offset = values_offset
                .iter()
                .find(|offset| self.offset_modified_by_bylabel_rule.get(**offset));

            anyhow::ensure!(
                collision_offset.is_none(),
                "The by-value rule is colliding with at least one by-label rule at offset {collision_offset:?}\n\
                by-value rule values offset(s) {values_offset:?}",
            )
        } else if let Some(rules) = self.rules.get(&(StringId::ZERO, StringId::ZERO)) {
            let collide_with_byvalue_rule = rules
                .iter()
                .find(|rule| is_overlapping(&rule.values_offset, values_offset));
            anyhow::ensure!(collide_with_byvalue_rule.is_none(),
                "The by-label rule (label name {label_name_str}, label value {label_value_str}) is colliding with a by-value rule on values offsets\n\
                Existing values offset(s) {collide_with_byvalue_rule:?}, new rule values offset(s) {values_offset:?}");
        }
        Ok(())
    }

    pub fn get(&self, k: &(StringId, StringId)) -> Option<&Vec<UpscalingRule>> {
        self.rules.get(k)
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    #[cfg(not(feature = "otel"))]
    pub(super) fn upscale_values(&self, values: &mut [i64], labels: &[Label]) {
        if self.is_empty() {
            return;
        }
        // get bylabel rules first (if any)
        let mut group_of_rules = labels
            .iter()
            .filter_map(|label| {
                self.get(&(
                    label.get_key(),
                    match label.get_value() {
                        LabelValue::Str(str) => *str,
                        LabelValue::Num { .. } => StringId::ZERO,
                    },
                ))
            })
            .collect::<Vec<&Vec<UpscalingRule>>>();

        // get byvalue rules if any
        if let Some(byvalue_rules) = self.get(&(StringId::ZERO, StringId::ZERO)) {
            group_of_rules.push(byvalue_rules);
        }

        group_of_rules.iter().for_each(|rules| {
            rules.iter().for_each(|rule| {
                let scale = rule.compute_scale(values);
                rule.values_offset.iter().for_each(|offset| {
                    values[*offset] = (values[*offset] as f64 * scale).round() as i64
                })
            })
        });
    }
}

/// Upscaling rule for a singleton (unpaired) observation.
///
/// `Poisson` is absent: it requires two values and is therefore only valid in a pair context.
/// `PoissonNonSampleTypeCount` needs no `sum_type` field because the singleton value *is* the sum.
#[cfg(feature = "otel")]
#[derive(Clone, Copy, Debug)]
pub enum OtelSingletonInfo {
    Proportional {
        scale: f64,
    },
    PoissonNonSampleTypeCount {
        count_value: u64,
        sampling_distance: u64,
    },
}

#[cfg(feature = "otel")]
impl OtelSingletonInfo {
    fn apply(&self, value: &mut i64) {
        match self {
            OtelSingletonInfo::Proportional { scale } => {
                *value = (*value as f64 * scale).round() as i64;
            }
            OtelSingletonInfo::PoissonNonSampleTypeCount {
                count_value,
                sampling_distance,
            } => {
                if *value == 0 || *count_value == 0 {
                    return;
                }
                let avg = *value as f64 / *count_value as f64;
                let scale = 1.0 / (1.0 - (-avg / *sampling_distance as f64).exp());
                *value = (*value as f64 * scale).round() as i64;
            }
        }
    }
}

/// Upscaling rule for a paired observation, stored under the lower-index [SampleType] of the
/// pair. Only [UpscalingInfo::Poisson] needs both values simultaneously; the other variants scale
/// each member independently and are handled as singletons instead.
#[cfg(feature = "otel")]
#[derive(Clone, Copy, Debug)]
pub struct OtelPoissonPairInfo {
    pub sum_type: SampleType,
    pub count_type: SampleType,
    pub sampling_distance: u64,
    /// Whether to write the computed scale back to the sum member.  The sum/count are always
    /// *read* to compute the scale, but the caller may have registered only a subset of the pair
    /// in `values_offset`, meaning only those members should be overwritten.
    pub scale_sum: bool,
    pub scale_count: bool,
}

#[cfg(feature = "otel")]
impl OtelPoissonPairInfo {
    /// Apply this rule in-place.
    fn apply(
        &self,
        st1: SampleType,
        v1: &mut i64,
        st2: SampleType,
        v2: &mut i64,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            (st1 == self.sum_type && st2 == self.count_type)
                || (st1 == self.count_type && st2 == self.sum_type),
            "OtelPoissonPairInfo::apply called with unexpected types: \
             got ({st1:?}, {st2:?}), expected ({:?}, {:?})",
            self.sum_type,
            self.count_type,
        );
        let (sum, count) = if self.sum_type == st1 {
            (*v1, *v2)
        } else {
            (*v2, *v1)
        };
        if sum == 0 || count == 0 {
            return Ok(());
        }
        let avg = sum as f64 / count as f64;
        let scale = 1.0 / (1.0 - (-avg / self.sampling_distance as f64).exp());
        let (sum_ref, count_ref) = if self.sum_type == st1 {
            (v1, v2)
        } else {
            (v2, v1)
        };
        if self.scale_sum {
            *sum_ref = (*sum_ref as f64 * scale).round() as i64;
        }
        if self.scale_count {
            *count_ref = (*count_ref as f64 * scale).round() as i64;
        }
        Ok(())
    }
}

/// Per-label-group rule storage, split so that singleton and pair upscaling never touch each
/// other's rules.
#[cfg(feature = "otel")]
#[derive(Default)]
struct OtelRuleBuckets {
    /// Rules for unpaired [SampleType]s, indexed by the type they scale.
    singleton: EnumMap<SampleType, Vec<OtelSingletonInfo>>,
    /// Rules for paired [SampleType]s, indexed by the canonical-first type of the pair.
    pair: EnumMap<SampleType, Vec<OtelPoissonPairInfo>>,
}

/// OTEL-style upscaling: same rule registration as [UpscalingRules] but upscaling operates on
/// individual ([SampleType], value) pairs rather than a full value vector.
///
/// Rules are routed at registration time into singleton or pair buckets based on the
/// [UpscalingInfo] variant: [UpscalingInfo::Poisson] → pair bucket (needs both values);
/// everything else → singleton bucket (needs only one value at a time).
///
/// Pair rules are stored under the lower-index [SampleType] of the pair, matching the key order
/// that [Observations] uses for `aggregated2` (it only inserts when `i < pair`). Callers must
/// therefore pass pairs to [upscale_pair] in the same lower-index-first order.
#[cfg(feature = "otel")]
#[derive(Default)]
pub struct OtelUpscalingRules {
    /// Used only for collision detection inside [OtelUpscalingRules::add].
    inner: UpscalingRules,
    /// `sample_types[i]` is the [SampleType] at value-vector index `i`.
    sample_types: std::sync::Arc<[SampleType]>,
    by_value: OtelRuleBuckets,
    by_label: FxIndexMap<(StringId, StringId), OtelRuleBuckets>,
}

#[cfg(feature = "otel")]
impl OtelUpscalingRules {
    pub fn new(sample_types: std::sync::Arc<[SampleType]>) -> Self {
        Self {
            sample_types,
            ..Default::default()
        }
    }

    /// Same signature as [UpscalingRules::add].
    ///
    /// Routing is determined solely by the [UpscalingInfo] variant:
    /// - [UpscalingInfo::Proportional] and [UpscalingInfo::PoissonNonSampleTypeCount] only need one
    ///   value at a time → stored as singleton rules for each target offset.
    /// - [UpscalingInfo::Poisson] needs sum and count simultaneously → stored as a single pair rule
    ///   under the canonical-first (lower-index) [SampleType] of the pair.
    pub fn add(
        &mut self,
        values_offset: &[usize],
        label_name: (&str, StringId),
        label_value: (&str, StringId),
        upscaling_info: UpscalingInfo,
        max_offset: usize,
    ) -> anyhow::Result<()> {
        self.inner.add(
            values_offset,
            label_name,
            label_value,
            upscaling_info,
            max_offset,
        )?;

        // Extract as a plain slice so the borrow checker sees it as disjoint from
        // by_value/by_label.
        let sample_types: &[SampleType] = &self.sample_types;

        let (name_id, val_id) = (label_name.1, label_value.1);
        let bucket = if name_id.is_zero() && val_id.is_zero() {
            &mut self.by_value
        } else {
            self.by_label.entry((name_id, val_id)).or_default()
        };

        match &upscaling_info {
            UpscalingInfo::Proportional { scale } => {
                // A single Proportional rule may target multiple offsets (e.g. scale both
                // CpuSamples and WallTime by the same factor). Each becomes an independent
                // singleton rule so upscale_singleton can apply it with a direct lookup.
                for &o in values_offset {
                    bucket.singleton[sample_types[o]]
                        .push(OtelSingletonInfo::Proportional { scale: *scale });
                }
            }
            UpscalingInfo::PoissonNonSampleTypeCount {
                sum_value_offset,
                count_value,
                sampling_distance,
            } => {
                anyhow::ensure!(
                    values_offset == [*sum_value_offset],
                    "PoissonNonSampleTypeCount rule: values_offset must be exactly [sum_value_offset ({sum_value_offset})], \
                     got {values_offset:?}",
                );
                let st = sample_types[*sum_value_offset];
                bucket.singleton[st].push(OtelSingletonInfo::PoissonNonSampleTypeCount {
                    count_value: *count_value,
                    sampling_distance: *sampling_distance,
                });
            }
            UpscalingInfo::Poisson {
                sum_value_offset,
                count_value_offset,
                sampling_distance,
            } => {
                let scale_sum = values_offset.contains(sum_value_offset);
                let scale_count = values_offset.contains(count_value_offset);
                anyhow::ensure!(
                    scale_sum || scale_count,
                    "Poisson rule: values_offset must contain at least one of \
                     sum_value_offset ({sum_value_offset}) or count_value_offset ({count_value_offset}), \
                     got {values_offset:?}",
                );
                for &o in values_offset {
                    anyhow::ensure!(
                        o == *sum_value_offset || o == *count_value_offset,
                        "Poisson rule: values_offset contains unexpected offset {o}; \
                         only sum_value_offset ({sum_value_offset}) and count_value_offset \
                         ({count_value_offset}) are valid, got {values_offset:?}",
                    );
                }
                let sum_type = sample_types[*sum_value_offset];
                let count_type = sample_types[*count_value_offset];
                anyhow::ensure!(sum_type != count_type);
                use enum_map::Enum as _;
                let cf = if sum_type.into_usize() < count_type.into_usize() {
                    sum_type
                } else {
                    count_type
                };
                bucket.pair[cf].push(OtelPoissonPairInfo {
                    sum_type,
                    count_type,
                    sampling_distance: *sampling_distance,
                    scale_sum,
                    scale_count,
                });
            }
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    fn label_key(label: &Label) -> (StringId, StringId) {
        (
            label.get_key(),
            match label.get_value() {
                LabelValue::Str(str) => *str,
                LabelValue::Num { .. } => StringId::ZERO,
            },
        )
    }

    /// Apply all matching upscaling rules to a full pprof-style value vector.
    ///
    /// `paired` is a snapshot of [Observations::paired_samples]: `paired[i]` is the profile index
    /// of `i`'s partner, or `None` if unpaired. The walk mirrors [Observations::add]: each pair
    /// is visited exactly once (when the canonical-first type drives the iteration), so this
    /// exercises [upscale_singleton] and [upscale_pair] on the same code path used by otel output.
    pub(super) fn upscale_values(
        &self,
        values: &mut [i64],
        paired: &[Option<usize>],
        labels: &[Label],
    ) -> anyhow::Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        use enum_map::Enum as _;
        for i in 0..values.len() {
            let st1 = self.sample_types[i];
            if let Some(partner) = paired[i] {
                let st2 = self.sample_types[partner];
                if st1.into_usize() < st2.into_usize() {
                    // upscale_pair applies pair rules (Poisson) then singleton rules for
                    // each member, so a single call handles all rule types for this pair.
                    let (v1, v2) =
                        self.upscale_pair(st1, values[i], st2, values[partner], labels)?;
                    values[i] = v1;
                    values[partner] = v2;
                }
                // else: this pair will be handled when the canonical-first index is reached
            } else {
                values[i] = self.upscale_singleton(st1, values[i], labels);
            }
        }
        Ok(())
    }

    /// Apply any matching upscaling rules to a single unpaired value.
    pub fn upscale_singleton(&self, st: SampleType, mut value: i64, labels: &[Label]) -> i64 {
        if self.is_empty() {
            return value;
        }
        for rule in &self.by_value.singleton[st] {
            rule.apply(&mut value);
        }
        for label in labels {
            if let Some(bucket) = self.by_label.get(&Self::label_key(label)) {
                for rule in &bucket.singleton[st] {
                    rule.apply(&mut value);
                }
            }
        }
        value
    }

    /// Apply any matching upscaling rules to a paired observation.
    ///
    /// Applies pair rules (Poisson, which needs both values simultaneously) first, then applies
    /// singleton rules (Proportional, PoissonNonSampleTypeCount) to each member independently.
    /// Callers do not need to follow this with separate [upscale_singleton] calls.
    ///
    /// `st1` must be the lower-index type of the pair (matching the key order used by
    /// [Observations::aggregated2], which is guaranteed by its `i < pair` insertion guard).
    pub fn upscale_pair(
        &self,
        st1: SampleType,
        mut v1: i64,
        st2: SampleType,
        mut v2: i64,
        labels: &[Label],
    ) -> anyhow::Result<(i64, i64)> {
        use enum_map::Enum as _;
        anyhow::ensure!(
            st1.into_usize() < st2.into_usize(),
            "upscale_pair: st1 ({st1:?}) must have a lower enum index than st2 ({st2:?})",
        );
        if self.is_empty() {
            return Ok((v1, v2));
        }
        for rule in &self.by_value.pair[st1] {
            rule.apply(st1, &mut v1, st2, &mut v2)?;
        }
        for label in labels {
            if let Some(bucket) = self.by_label.get(&Self::label_key(label)) {
                for rule in &bucket.pair[st1] {
                    rule.apply(st1, &mut v1, st2, &mut v2)?;
                }
            }
        }
        v1 = self.upscale_singleton(st1, v1, labels);
        v2 = self.upscale_singleton(st2, v2, labels);
        Ok((v1, v2))
    }
}
