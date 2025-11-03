// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Utc};

use crate::rules_based::{
    error::{EvaluationError, EvaluationFailure},
    ufc::{
        Allocation, Assignment, AssignmentReason, CompiledFlagsConfig, Flag, Shard, Split,
        Timestamp, VariationType,
    },
    Configuration, EvaluationContext,
};

/// Evaluate the specified feature flag for the given subject and return assigned variation and
/// an optional assignment event for logging.
pub fn get_assignment(
    configuration: Option<&Configuration>,
    flag_key: &str,
    subject: &EvaluationContext,
    expected_type: Option<VariationType>,
    now: DateTime<Utc>,
) -> Result<Option<Assignment>, EvaluationError> {
    let Some(config) = configuration else {
        log::trace!(
            flag_key,
            targeting_key = subject.targeting_key();
            "returning default assignment because of: {}", EvaluationFailure::ConfigurationMissing);
        return Ok(None);
    };

    config.eval_flag(flag_key, subject, expected_type, now)
}

impl Configuration {
    pub fn eval_flag(
        &self,
        flag_key: &str,
        context: &EvaluationContext,
        expected_type: Option<VariationType>,
        now: DateTime<Utc>,
    ) -> Result<Option<Assignment>, EvaluationError> {
        let result = self
            .flags
            .compiled
            .eval_flag(flag_key, context, expected_type, now);

        match result {
            Ok(assignment) => {
                log::trace!(
                flag_key,
                targeting_key = context.targeting_key(),
                assignment:serde = assignment.value;
                "evaluated a flag");
                Ok(Some(assignment))
            }

            Err(EvaluationFailure::ConfigurationMissing) => {
                log::warn!(
                flag_key,
                targeting_key = context.targeting_key();
                "evaluating a flag before flags configuration has been fetched");
                Ok(None)
            }

            Err(EvaluationFailure::Error(err)) => {
                log::warn!(
                    flag_key,
                    targeting_key = context.targeting_key();
                    "error occurred while evaluating a flag: {err}",
                );
                Err(err)
            }

            // Non-Error failures are considered normal conditions and usually don't need extra
            // attention, so we remap them to Ok(None) before returning to the user.
            Err(err) => {
                log::trace!(
                    flag_key,
                    targeting_key = context.targeting_key();
                    "returning default assignment because of: {err}");
                Ok(None)
            }
        }
    }
}

impl CompiledFlagsConfig {
    /// Evaluate the flag for the given subject, expecting `expected_type` type.
    fn eval_flag(
        &self,
        flag_key: &str,
        subject: &EvaluationContext,
        expected_type: Option<VariationType>,
        now: DateTime<Utc>,
    ) -> Result<Assignment, EvaluationFailure> {
        let flag = self.get_flag(flag_key)?;

        if let Some(ty) = expected_type {
            flag.verify_type(ty)?;
        }

        flag.eval(subject, now)
    }

    fn get_flag(&self, flag_key: &str) -> Result<&Flag, EvaluationFailure> {
        self.flags
            .get(flag_key)
            .ok_or(EvaluationFailure::FlagUnrecognizedOrDisabled)?
            .as_ref()
            .map_err(Clone::clone)
    }
}

impl Flag {
    fn verify_type(&self, ty: VariationType) -> Result<(), EvaluationFailure> {
        if self.variation_type == ty {
            Ok(())
        } else {
            Err(EvaluationFailure::Error(EvaluationError::TypeMismatch {
                expected: ty,
                found: self.variation_type,
            }))
        }
    }

    fn eval(
        &self,
        subject: &EvaluationContext,
        now: DateTime<Utc>,
    ) -> Result<Assignment, EvaluationFailure> {
        let Some((allocation, (split, reason))) = self.allocations.iter().find_map(|allocation| {
            let result = allocation.get_matching_split(subject, now);
            result
                .ok()
                .map(|(split, reason)| (allocation, (split, reason)))
        }) else {
            return Err(EvaluationFailure::DefaultAllocationNull);
        };

        let value = split.value.clone();

        Ok(Assignment {
            value,
            variation_key: split.variation_key.clone(),
            allocation_key: allocation.key.clone(),
            extra_logging: split.extra_logging.clone(),
            reason,
            do_log: allocation.do_log,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum AllocationNonMatchReason {
    BeforeStartDate,
    AfterEndDate,
    FailingRule,
    TrafficExposureMiss,
}

impl Allocation {
    fn get_matching_split(
        &self,
        subject: &EvaluationContext,
        now: Timestamp,
    ) -> Result<(&Split, AssignmentReason), AllocationNonMatchReason> {
        if self.start_at.is_some_and(|t| now < t) {
            return Err(AllocationNonMatchReason::BeforeStartDate);
        }
        if self.end_at.is_some_and(|t| now > t) {
            return Err(AllocationNonMatchReason::AfterEndDate);
        }

        let is_allowed_by_rules =
            self.rules.is_empty() || self.rules.iter().any(|rule| rule.eval(subject));
        if !is_allowed_by_rules {
            return Err(AllocationNonMatchReason::FailingRule);
        }

        let split = self
            .splits
            .iter()
            .find(|split| {
                let matches = split.matches(subject.targeting_key());
                matches
            })
            .ok_or(AllocationNonMatchReason::TrafficExposureMiss)?;

        // Determine the reason for assignment
        let reason = if !self.rules.is_empty() || self.start_at.is_some() || self.end_at.is_some() {
            AssignmentReason::TargetingMatch
        } else if self.splits.len() == 1 && self.splits[0].shards.is_empty() {
            AssignmentReason::Static
        } else {
            AssignmentReason::Split
        };

        Ok((split, reason))
    }
}

impl Split {
    /// Return `true` if `targeting_key` matches the given split.
    ///
    /// To match a split, subject must match all underlying shards.
    fn matches(&self, targeting_key: &str) -> bool {
        self.shards.iter().all(|shard| shard.matches(targeting_key))
    }
}

impl Shard {
    /// Return `true` if `targeting_key` matches the given shard.
    fn matches(&self, targeting_key: &str) -> bool {
        let h = self.sharder.shard(&[targeting_key]);

        self.ranges.iter().any(|range| range.contains(h))
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fs::File, sync::Arc};

    use chrono::Utc;
    use serde::{Deserialize, Serialize};

    use crate::rules_based::{
        eval::get_assignment,
        ufc::{AssignmentValue, UniversalFlagConfig, VariationType},
        Attribute, Configuration, EvaluationContext, Str,
    };

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TestCase {
        flag: String,
        variation_type: VariationType,
        default_value: serde_json::Value,
        targeting_key: Str,
        attributes: Arc<HashMap<Str, Attribute>>,
        result: TestResult,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct TestResult {
        value: serde_json::Value,
    }

    // Include the SDK tests generated by build.rs at compile time
    // The build script automatically discovers all test files in tests/data/tests/
    // and generates a separate test function for each one
    include!(concat!(env!("OUT_DIR"), "/sdk_tests.rs"));
}
