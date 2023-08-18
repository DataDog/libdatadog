// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present Datadog, Inc.

use super::{StringId, ValueType};
use crate::profile::api::UpscalingInfo;
use crate::profile::pprof;
use crate::profile::FxIndexMap;

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

    pub fn check_collisions(
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

    pub fn get(&self, k: &(StringId, StringId)) -> Option<&Vec<UpscalingRule>> {
        self.rules.get(k)
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    // TODO: Consider whether to use the internal Label here instead
    pub fn upscale_values(
        &self,
        values: &[i64],
        labels: &[pprof::Label],
        sample_types: &Vec<ValueType>,
    ) -> anyhow::Result<Vec<i64>> {
        let mut new_values = values.to_vec();

        if !self.is_empty() {
            let mut values_to_update: Vec<usize> = vec![0; sample_types.len()];

            // get bylabel rules first (if any)
            let mut group_of_rules = labels
                .iter()
                .filter_map(|label| self.get(&(StringId::new(label.key), StringId::new(label.str))))
                .collect::<Vec<&Vec<UpscalingRule>>>();

            // get byvalue rules if any
            if let Some(byvalue_rules) = self.get(&(StringId::ZERO, StringId::ZERO)) {
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
}
