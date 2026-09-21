// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::api::UpscalingInfo;
use anyhow::Context;

#[derive(Debug)]
pub struct UpscalingRule {
    upscaling_info: UpscalingInfo,
    values_offset: Vec<usize>,
    // Keep raw values for late endpoints and overlapping rules, whose order matters.
    deferred: bool,
    poisson_corrections: FxIndexMap<Sample, Vec<f64>>,
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

    pub fn new(values_offset: Vec<usize>, upscaling_info: UpscalingInfo, deferred: bool) -> Self {
        Self {
            values_offset,
            upscaling_info,
            deferred,
            poisson_corrections: FxIndexMap::default(),
        }
    }

    fn is_eager_poisson(&self) -> bool {
        !self.deferred && !matches!(self.upscaling_info, UpscalingInfo::Proportional { .. })
    }

    fn add_correction(&mut self, sample: Sample, values: &[i64]) {
        let correction_scale = self.compute_scale(values) - 1.0;
        if correction_scale == 0.0 {
            return;
        }
        let corrections = self
            .poisson_corrections
            .entry(sample)
            .or_insert_with(|| vec![0.0; self.values_offset.len()]);
        for (correction, offset) in corrections.iter_mut().zip(&self.values_offset) {
            *correction += values[*offset] as f64 * correction_scale;
        }
    }
}

#[derive(Default)]
pub struct UpscalingRules {
    rules: FxIndexMap<(StringId, StringId), Vec<UpscalingRule>>,
    // Reused for each incoming sample, not retained per stack or rule.
    weighted_values: Vec<Option<f64>>,
    endpoint_label: Option<StringId>,
    has_aggregated_samples: bool,
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
        // Different values of one label cannot match the same sample. Rules on
        // different labels can overlap; keep their raw values and application order.
        let overlaps = |key: StringId, rule: &UpscalingRule| {
            key != label_name.1
                && rule
                    .values_offset
                    .iter()
                    .any(|offset| new_values_offset.contains(offset))
        };
        anyhow::ensure!(
            !self.has_aggregated_samples
                || !self.rules.iter().any(|((key, _), rules)| {
                    rules
                        .iter()
                        .any(|rule| overlaps(*key, rule) && rule.is_eager_poisson())
                }),
            "Rules overlapping Poisson targets must be registered before adding samples"
        );
        let mut deferred = label_name.0 == "trace endpoint";
        if deferred {
            self.endpoint_label = Some(label_name.1);
        }
        for ((key, _), rules) in &mut self.rules {
            for rule in rules {
                if overlaps(*key, rule) {
                    rule.deferred = true;
                    deferred = true;
                }
            }
        }
        let rule = UpscalingRule::new(new_values_offset, upscaling_info, deferred);

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

    pub fn add_sample(
        &mut self,
        sample: Sample,
        values: &[i64],
        labels: impl Iterator<Item = anyhow::Result<Label>>,
        observations: &mut Observations,
    ) -> anyhow::Result<()> {
        self.weighted_values.resize(values.len(), None);
        self.weighted_values.fill(None);
        let keys = labels.map(|label| label.map(|label| Self::label_key(&label)));
        for key in keys.chain(std::iter::once(Ok((StringId::ZERO, StringId::ZERO)))) {
            let key = key?;
            if Some(key.0) == self.endpoint_label {
                continue;
            }
            if let Some(rules) = self.rules.get_mut(&key) {
                for rule in rules {
                    if matches!(rule.upscaling_info, UpscalingInfo::Proportional { .. }) {
                        continue;
                    }
                    if rule.deferred {
                        rule.add_correction(sample, values);
                    } else {
                        // Always read the original integers, not another rule's weighted values.
                        let scale = rule.compute_scale(values);
                        for &offset in &rule.values_offset {
                            self.weighted_values[offset] = Some(values[offset] as f64 * scale);
                        }
                    }
                }
            }
        }

        if let Some(endpoint_label) = self.endpoint_label {
            // ponytail: late endpoints retain per-rule corrections; share identical
            // formulas if endpoint-specific rules become a significant memory cost.
            for ((key, _), rules) in &mut self.rules {
                if *key != endpoint_label {
                    continue;
                }
                for rule in rules {
                    if matches!(rule.upscaling_info, UpscalingInfo::Proportional { .. }) {
                        continue;
                    }
                    rule.add_correction(sample, values);
                }
            }
        }

        self.has_aggregated_samples = true;
        observations.add_with(sample, None, values, |totals| {
            for (offset, total) in totals.iter_mut().enumerate() {
                if let Some(value) = self.weighted_values[offset] {
                    *total = (f64::from_bits(total.cast_unsigned()) + value)
                        .to_bits()
                        .cast_signed();
                } else {
                    *total = total.saturating_add(values[offset]);
                }
            }
        })
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

    pub fn upscale_values(
        &self,
        values: &mut [i64],
        labels: &[Label],
        aggregated_sample: Option<Sample>,
    ) {
        if self.is_empty() {
            return;
        }
        // get bylabel rules first (if any)
        let mut group_of_rules = labels
            .iter()
            .filter_map(|label| self.get(&Self::label_key(label)))
            .collect::<Vec<&Vec<UpscalingRule>>>();

        // get byvalue rules if any
        if let Some(byvalue_rules) = self.get(&(StringId::ZERO, StringId::ZERO)) {
            group_of_rules.push(byvalue_rules);
        }

        if aggregated_sample.is_some() {
            // Decode each float slot once, even if several rules target it.
            for (offset, value) in values.iter_mut().enumerate() {
                if group_of_rules
                    .iter()
                    .flat_map(|rules| rules.iter())
                    .any(|rule| rule.is_eager_poisson() && rule.values_offset.contains(&offset))
                {
                    // Round only the final total. The cast saturates to i64's range.
                    *value = f64::from_bits(value.cast_unsigned()).round() as i64;
                }
            }
        }

        group_of_rules.iter().for_each(|rules| {
            rules.iter().for_each(|rule| {
                if let Some(sample) = aggregated_sample {
                    if rule.is_eager_poisson() {
                        return;
                    }
                    if !matches!(rule.upscaling_info, UpscalingInfo::Proportional { .. }) {
                        if let Some(corrections) = rule.poisson_corrections.get(&sample) {
                            for (offset, correction) in rule.values_offset.iter().zip(corrections) {
                                // Round once; the cast saturates values outside i64's range.
                                values[*offset] =
                                    (values[*offset] as f64 + correction).round() as i64;
                            }
                        }
                        return;
                    }
                }
                let scale = rule.compute_scale(values);
                rule.values_offset.iter().for_each(|offset| {
                    values[*offset] = (values[*offset] as f64 * scale).round() as i64
                })
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collections::identifiable::Id;

    #[test]
    fn label_rules_keep_only_weighted_totals() {
        let mut rules = UpscalingRules::default();
        let mut observations = Observations::new(3);
        let key = StringId::from_offset(1);
        for i in 0..32 {
            rules
                .add(
                    &[0, 1],
                    ("class", key),
                    ("value", StringId::from_offset(i + 2)),
                    UpscalingInfo::Poisson {
                        sum_value_offset: 1,
                        count_value_offset: 0,
                        sampling_distance: 100,
                    },
                    3,
                )
                .unwrap();
        }

        let wall_time = (1_i64 << 53) + 1;
        for label in 0..33 {
            let labels = [Label::str(key, StringId::from_offset(label + 2))];
            for stack in 0..100 {
                let sample = Sample::new(
                    LabelSetId::from_offset(label),
                    StackTraceId::from_offset(stack),
                );
                // Start with a CPU-only sample to check that zero float slots
                // have the same layout as the allocation samples that follow.
                for values in [[0, 0, wall_time], [1, 10, 0], [1, 200, 0]] {
                    rules
                        .add_sample(
                            sample,
                            &values,
                            labels.iter().copied().map(Ok),
                            &mut observations,
                        )
                        .unwrap();
                }
            }
        }
        assert_eq!(observations.aggregated_samples_count(), 3300);
        assert!(rules
            .rules
            .values()
            .flatten()
            .all(|rule| rule.poisson_corrections.is_empty()));
        assert_eq!(rules.weighted_values.len(), 3);

        for (sample, timestamp, mut values) in observations.try_into_iter().unwrap() {
            assert!(timestamp.is_none());
            let label = sample.labels.to_offset();
            let labels = [Label::str(key, StringId::from_offset(label + 2))];
            rules.upscale_values(&mut values, &labels, Some(sample));
            assert_eq!(
                values,
                if label == 32 {
                    vec![2, 210, wall_time]
                } else {
                    vec![12, 336, wall_time]
                }
            );
        }
    }
}
