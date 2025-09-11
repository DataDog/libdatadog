// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use datadog_profiling_protobuf::{Label, Record, StringOffset, NO_OPT_ZERO};
use std::num::NonZeroU64;

/// Labels are grouped by the (key, value), for example using strings (but of
/// course we're actually using offsets to refer to those strings):
///  - `("exception type", "OutOfBoundsException")`
///  - `("exception type", "TimeoutException")`
///
/// For rules which don't use label groups, use the default group `(0, 0)`.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct GroupByLabel {
    pub key: StringOffset,
    pub value: StringOffset,
}

#[derive(Clone, Copy, Debug)]
pub struct ProportionalUpscalingRule {
    pub group_by_label: GroupByLabel,
    pub scale: f64,
}

fn scale_values(values: &mut [i64], scale: f64) {
    for v in values.iter_mut() {
        *v = ((*v as f64) * scale).round() as i64;
    }
}

impl ProportionalUpscalingRule {
    pub fn scale(&self, values: &mut [i64], labels: &[Record<Label, 3, { NO_OPT_ZERO }>]) {
        let ProportionalUpscalingRule {
            group_by_label,
            scale,
        } = self;
        let scale = *scale;
        if scale != 1.0 {
            // Simple case: no need to do any filtering.
            if group_by_label.key.is_zero() && group_by_label.value.is_zero() {
                scale_values(values, scale);
            } else {
                let matched = labels
                    .iter()
                    .find(|label| {
                        let label = &label.value;
                        label.key.value == group_by_label.key
                            && label.str.value == group_by_label.value
                    })
                    .is_some();
                if matched {
                    scale_values(values, scale);
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PoissonUpscalingRule {
    pub sum_offset: usize,
    pub count_offset: usize,
    pub sampling_distance: NonZeroU64,
}

impl PoissonUpscalingRule {
    pub fn compute_scale(&self, values: &[i64]) -> f64 {
        let sum_offset = self.sum_offset as usize;
        let count_offset = self.count_offset as usize;
        let sum = values[sum_offset];
        let count = values[count_offset];
        let sampling_distance = self.sampling_distance.get() as f64;

        // [0] / [1]
        let avg = sum as f64 / count as f64;
        1_f64 / (1_f64 - (-avg / sampling_distance).exp())
    }

    pub fn scale(&self, values: &mut [i64]) {
        let scale = self.compute_scale(values);
        if scale != 1.0 {
            scale_values(values, scale);
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum UpscalingRule {
    ProportionalUpscalingRule(ProportionalUpscalingRule),
    PoissonUpscalingRule(PoissonUpscalingRule),
}

impl UpscalingRule {
    pub fn scale(&self, values: &mut [i64], labels: &[Record<Label, 3, { NO_OPT_ZERO }>]) {
        match self {
            UpscalingRule::ProportionalUpscalingRule(rule) => rule.scale(values, labels),
            UpscalingRule::PoissonUpscalingRule(rule) => rule.scale(values),
        }
    }
}

// TODO: turn these into test cases
// {key: "Duration bucket", value: "0 - 9 ms"} -> UpscalingInfo::Proportional
// {key: "Wait duration bucket", value: "0 - 9 ms"} -> UpscalingInfo::Proportional

// {} -> UpscalingInfo::Poisson
