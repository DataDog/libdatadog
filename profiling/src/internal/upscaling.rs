// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::api::UpscalingInfo;
use anyhow::Context;

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
            UpscalingInfo::PoissonCount {
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

    pub fn upscale_values(&self, values: &mut [i64], labels: &[Label]) -> anyhow::Result<()> {
        if !self.is_empty() {
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

        Ok(())
    }
}
