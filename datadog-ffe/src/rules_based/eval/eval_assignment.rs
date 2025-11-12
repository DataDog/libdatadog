// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Utc};

use crate::rules_based::{
    error::EvaluationError,
    ufc::{Allocation, Assignment, AssignmentReason, CompiledFlagsConfig, Flag, Shard, Split},
    Configuration, EvaluationContext, ExpectedFlagType, Timestamp,
};

/// Evaluate the specified feature flag for the given subject and return assigned variation and
/// an optional assignment event for logging.
pub fn get_assignment(
    configuration: Option<&Configuration>,
    flag_key: &str,
    subject: &EvaluationContext,
    expected_type: ExpectedFlagType,
    now: DateTime<Utc>,
) -> Result<Assignment, EvaluationError> {
    let Some(config) = configuration else {
        log::trace!(
            flag_key,
            targeting_key = subject.targeting_key();
            "returning default assignment because of: {}", EvaluationError::ConfigurationMissing);
        return Err(EvaluationError::ConfigurationMissing);
    };

    config.eval_flag(flag_key, subject, expected_type, now)
}

impl Configuration {
    pub fn eval_flag(
        &self,
        flag_key: &str,
        context: &EvaluationContext,
        expected_type: ExpectedFlagType,
        now: DateTime<Utc>,
    ) -> Result<Assignment, EvaluationError> {
        let result = self
            .flags
            .compiled
            .eval_flag(flag_key, context, expected_type, now);

        match &result {
            Ok(assignment) => {
                log::trace!(
                    flag_key,
                    targeting_key = context.targeting_key(),
                    assignment:? = assignment.value;
                    "evaluated a flag");
            }

            Err(err) => {
                log::trace!(
                    flag_key,
                    targeting_key = context.targeting_key();
                    "returning default assignment because of: {err}");
            }
        }

        result
    }
}

impl CompiledFlagsConfig {
    /// Evaluate the flag for the given subject, expecting `expected_type` type.
    fn eval_flag(
        &self,
        flag_key: &str,
        subject: &EvaluationContext,
        expected_type: ExpectedFlagType,
        now: DateTime<Utc>,
    ) -> Result<Assignment, EvaluationError> {
        self.get_flag(flag_key)?.eval(subject, expected_type, now)
    }

    fn get_flag(&self, flag_key: &str) -> Result<&Flag, EvaluationError> {
        self.flags
            .get(flag_key)
            .ok_or(EvaluationError::FlagUnrecognizedOrDisabled)?
            .as_ref()
            .map_err(Clone::clone)
    }
}

impl Flag {
    fn eval(
        &self,
        context: &EvaluationContext,
        expected_type: ExpectedFlagType,
        now: DateTime<Utc>,
    ) -> Result<Assignment, EvaluationError> {
        if !expected_type.is_compatible(self.variation_type.into()) {
            return Err(EvaluationError::TypeMismatch {
                expected: expected_type,
                found: self.variation_type.into(),
            });
        }

        let (allocation, split, reason) = self.find_allocation(context, now)?;

        Ok(Assignment {
            value: split.value.clone(),
            variation_key: split.variation_key.clone(),
            allocation_key: allocation.key.clone(),
            extra_logging: split.extra_logging.clone(),
            reason,
            do_log: allocation.do_log,
        })
    }

    fn find_allocation(
        &self,
        context: &EvaluationContext,
        now: DateTime<Utc>,
    ) -> Result<(&Allocation, &Split, AssignmentReason), EvaluationError> {
        for allocation in &self.allocations {
            if let Some((split, reason)) = allocation.get_matching_split(context, now)? {
                return Ok((allocation, split, reason));
            }
        }

        Err(EvaluationError::DefaultAllocationNull)
    }
}

impl Allocation {
    fn get_matching_split(
        &self,
        context: &EvaluationContext,
        now: Timestamp,
    ) -> Result<Option<(&Split, AssignmentReason)>, EvaluationError> {
        if self.start_at.is_some_and(|t| now < t) {
            return Ok(None);
        }
        if self.end_at.is_some_and(|t| now > t) {
            return Ok(None);
        }

        let is_allowed_by_rules =
            self.rules.is_empty() || self.rules.iter().any(|rule| rule.eval(context));
        if !is_allowed_by_rules {
            return Ok(None);
        }

        let Some(split) = self.find_split(context)? else {
            return Ok(None);
        };

        // Determine the reason for assignment
        let reason = if !self.rules.is_empty() || self.start_at.is_some() || self.end_at.is_some() {
            AssignmentReason::TargetingMatch
        } else if self.splits.len() == 1 && self.splits[0].shards.is_empty() {
            AssignmentReason::Static
        } else {
            AssignmentReason::Split
        };

        Ok(Some((split, reason)))
    }

    fn find_split(&self, subject: &EvaluationContext) -> Result<Option<&Split>, EvaluationError> {
        let targeting_key = subject.targeting_key().map(|it| it.as_str());

        for split in &self.splits {
            if split.matches(targeting_key)? {
                return Ok(Some(split));
            }
        }

        Ok(None)
    }
}

impl Split {
    /// Return `true` if `targeting_key` matches the given split.
    ///
    /// To match a split, subject must match all underlying shards.
    fn matches(&self, targeting_key: Option<&str>) -> Result<bool, EvaluationError> {
        if self.shards.is_empty() {
            return Ok(true);
        }

        let Some(targeting_key) = targeting_key else {
            return Err(EvaluationError::TargetingKeyMissing);
        };

        Ok(self.shards.iter().all(|shard| shard.matches(targeting_key)))
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
    use std::{
        collections::HashMap,
        fs::{self, File},
        sync::Arc,
    };

    use chrono::Utc;
    use serde::{Deserialize, Serialize};

    use crate::rules_based::{
        eval::get_assignment,
        ufc::{AssignmentValue, UniversalFlagConfig},
        Attribute, Configuration, EvaluationContext, FlagType, Str,
    };

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TestCase {
        flag: String,
        variation_type: FlagType,
        default_value: Arc<serde_json::value::RawValue>,
        targeting_key: Option<Str>,
        attributes: Arc<HashMap<Str, Attribute>>,
        result: TestResult,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct TestResult {
        value: Arc<serde_json::value::RawValue>,
    }

    #[test]
    #[cfg_attr(miri, ignore)] // this test is way too slow on miri
    fn evaluation_sdk_test_data() {
        let _ = env_logger::builder().is_test(true).try_init();

        let config =
            UniversalFlagConfig::from_json(std::fs::read("tests/data/flags-v1.json").unwrap())
                .unwrap();
        let config = Configuration::from_server_response(config);
        let now = Utc::now();

        for entry in fs::read_dir("tests/data/tests/").unwrap() {
            let entry = entry.unwrap();
            println!("Processing test file: {:?}", entry.path());

            let f = File::open(entry.path()).unwrap();
            let test_cases: Vec<TestCase> = serde_json::from_reader(f).unwrap();

            for test_case in test_cases {
                let default_assignment = AssignmentValue::from_wire(
                    test_case.variation_type.into(),
                    test_case.default_value,
                )
                .unwrap();

                print!("test subject {:?} ... ", test_case.targeting_key);
                let subject = EvaluationContext::new(test_case.targeting_key, test_case.attributes);
                let result = get_assignment(
                    Some(&config),
                    &test_case.flag,
                    &subject,
                    test_case.variation_type.into(),
                    now,
                );

                let result_assingment = result
                    .as_ref()
                    .map(|assignment| &assignment.value)
                    .unwrap_or(&default_assignment);
                let expected_assignment = AssignmentValue::from_wire(
                    test_case.variation_type.into(),
                    test_case.result.value,
                )
                .unwrap();

                assert_eq!(result_assingment, &expected_assignment);
                println!("ok");
            }
        }
    }
}
