use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::rules_based::{
    Attributes, Configuration, Str,
    error::{EvaluationError, EvaluationFailure},
    events::AssignmentEvent,
    ufc::{
        Allocation, Assignment, AssignmentReason, CompiledFlagsConfig, Flag, Shard, Split,
        Timestamp, VariationType,
    },
};

use super::{
    eval_visitor::{
        EvalAllocationVisitor, EvalAssignmentVisitor, EvalRuleVisitor, EvalSplitVisitor,
        NoopEvalVisitor,
    },
    subject::Subject,
};

/// Evaluate the specified feature flag for the given subject and return assigned variation and
/// an optional assignment event for logging.
pub fn get_assignment(
    configuration: Option<&Configuration>,
    flag_key: &str,
    subject_key: &Str,
    subject_attributes: &Arc<Attributes>,
    expected_type: Option<VariationType>,
    now: DateTime<Utc>,
) -> Result<Option<Assignment>, EvaluationError> {
    get_assignment_with_visitor(
        configuration,
        &mut NoopEvalVisitor,
        flag_key,
        subject_key,
        subject_attributes,
        expected_type,
        now,
    )
}

// Exposed for use in bandit evaluation.
pub(super) fn get_assignment_with_visitor<V: EvalAssignmentVisitor>(
    configuration: Option<&Configuration>,
    visitor: &mut V,
    flag_key: &str,
    subject_key: &Str,
    subject_attributes: &Arc<Attributes>,
    expected_type: Option<VariationType>,
    now: DateTime<Utc>,
) -> Result<Option<Assignment>, EvaluationError> {
    let result = if let Some(config) = configuration {
        visitor.on_configuration(config);

        config.flags.compiled.eval_flag(
            visitor,
            flag_key,
            subject_key,
            subject_attributes,
            expected_type,
            now,
        )
    } else {
        Err(EvaluationFailure::ConfigurationMissing)
    };

    visitor.on_result(&result);

    match result {
        Ok(assignment) => {
            log::trace!(target: "eppo",
                    flag_key,
                    subject_key,
                    assignment:serde = assignment.value;
                    "evaluated a flag");
            Ok(Some(assignment))
        }

        Err(EvaluationFailure::ConfigurationMissing) => {
            log::warn!(target: "eppo",
                           flag_key,
                           subject_key;
                           "evaluating a flag before Eppo configuration has been fetched");
            Ok(None)
        }

        Err(EvaluationFailure::Error(err)) => {
            log::warn!(target: "eppo",
                       flag_key,
                       subject_key;
                       "error occurred while evaluating a flag: {err}",
            );
            Err(err)
        }

        // Non-Error failures are considered normal conditions and usually don't need extra
        // attention, so we remap them to Ok(None) before returning to the user.
        Err(err) => {
            log::trace!(target: "eppo",
                           flag_key,
                           subject_key;
                           "returning default assignment because of: {err}");
            Ok(None)
        }
    }
}

impl CompiledFlagsConfig {
    /// Evaluate the flag for the given subject, expecting `expected_type` type.
    fn eval_flag<V: EvalAssignmentVisitor>(
        &self,
        visitor: &mut V,
        flag_key: &str,
        subject_key: &Str,
        subject_attributes: &Arc<Attributes>,
        expected_type: Option<VariationType>,
        now: DateTime<Utc>,
    ) -> Result<Assignment, EvaluationFailure> {
        let flag = self.get_flag(flag_key)?;

        visitor.on_flag_configuration(flag);

        if let Some(ty) = expected_type {
            flag.verify_type(ty)?;
        }

        flag.eval(visitor, subject_key, subject_attributes, now)
    }

    fn get_flag(&self, flag_key: &str) -> Result<&Flag, EvaluationFailure> {
        let flag = self
            .flags
            .get(flag_key)
            .ok_or(EvaluationFailure::FlagUnrecognizedOrDisabled)?
            .as_ref()
            .map_err(Clone::clone)?;
        Ok(flag)
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

    fn eval<V: EvalAssignmentVisitor>(
        &self,
        visitor: &mut V,
        subject_key: &Str,
        subject_attributes: &Arc<Attributes>,
        now: DateTime<Utc>,
    ) -> Result<Assignment, EvaluationFailure> {
        let subject = Subject::new(subject_key.clone(), subject_attributes.clone());

        let Some((allocation, (split, reason))) = self.allocations.iter().find_map(|allocation| {
            let mut visitor = visitor.visit_allocation(allocation);
            let result = allocation.get_matching_split(&mut visitor, &subject, now);
            visitor.on_result(result.as_ref().map(|(split, _)| *split).map_err(|e| *e));
            result
                .ok()
                .map(|(split, reason)| (allocation, (split, reason)))
        }) else {
            return Err(EvaluationFailure::DefaultAllocationNull);
        };

        let (value, event_base) = split.result.clone()?;

        Ok(Assignment {
            value,
            variation_key: split.variation_key.clone(),
            allocation_key: allocation.key.clone(),
            extra_logging: split.extra_logging.clone(),
            reason,
            event: event_base.map(|base| AssignmentEvent {
                base,
                subject: subject_key.clone(),
                subject_attributes: subject_attributes.clone(),
                timestamp: now,
            }),
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
    fn get_matching_split<V: EvalAllocationVisitor>(
        &self,
        visitor: &mut V,
        subject: &Subject,
        now: Timestamp,
    ) -> Result<(&Split, AssignmentReason), AllocationNonMatchReason> {
        if self.start_at.is_some_and(|t| now < t) {
            return Err(AllocationNonMatchReason::BeforeStartDate);
        }
        if self.end_at.is_some_and(|t| now > t) {
            return Err(AllocationNonMatchReason::AfterEndDate);
        }

        let is_allowed_by_rules = self.rules.is_empty()
            || self.rules.iter().any(|rule| {
                let mut visitor = visitor.visit_rule(rule);
                let result = rule.eval(&mut visitor, subject);
                visitor.on_result(result);
                result
            });
        if !is_allowed_by_rules {
            return Err(AllocationNonMatchReason::FailingRule);
        }

        let split = self
            .splits
            .iter()
            .find(|split| {
                let mut visitor = visitor.visit_split(split);
                let matches = split.matches(&mut visitor, subject.key());
                visitor.on_result(matches);
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
    /// Return `true` if `subject_key` matches the given split.
    ///
    /// To match a split, subject must match all underlying shards.
    fn matches<V: EvalSplitVisitor>(&self, visitor: &mut V, subject_key: &str) -> bool {
        self.shards
            .iter()
            .all(|shard| shard.matches(visitor, subject_key))
    }
}

impl Shard {
    /// Return `true` if `subject_key` matches the given shard.
    fn matches<V: EvalSplitVisitor>(&self, visitor: &mut V, subject_key: &str) -> bool {
        let h = self.sharder.shard(&[subject_key]);

        let matches = self.ranges.iter().any(|range| range.contains(h));
        visitor.on_shard_eval(self, h, matches);
        matches
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        sync::Arc,
    };

    use chrono::Utc;
    use serde::{Deserialize, Serialize};

    use crate::rules_based::{
        Attributes, Configuration, SdkMetadata, Str,
        eval::get_assignment,
        ufc::{AssignmentValue, UniversalFlagConfig, VariationType},
    };

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TestCase {
        flag: String,
        variation_type: VariationType,
        default_value: serde_json::Value,
        targeting_key: Str,
        attributes: Arc<Attributes>,
        result: TestResult,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct TestResult {
        value: serde_json::Value,
    }

    #[test]
    fn evaluation_sdk_test_data() {
        let _ = env_logger::builder().is_test(true).try_init();

        let config = UniversalFlagConfig::from_json(
            SdkMetadata {
                name: "test",
                version: "0.1.0",
            },
            {
                let path = if std::path::Path::new("tests/data/flags-v1.json").exists() {
                    "tests/data/flags-v1.json"
                } else {
                    "domains/ffe/libs/flagging/rust/evaluation/tests/data/flags-v1.json"
                };
                std::fs::read(path).unwrap()
            },
        )
        .unwrap();
        let config = Configuration::from_server_response(config);
        let now = Utc::now();

        let test_dir = if std::path::Path::new("tests/data/tests/").exists() {
            "tests/data/tests/"
        } else {
            "domains/ffe/libs/flagging/rust/evaluation/tests/data/tests/"
        };
        for entry in fs::read_dir(test_dir).unwrap() {
            let entry = entry.unwrap();
            println!("Processing test file: {:?}", entry.path());

            let f = File::open(entry.path()).unwrap();
            let test_cases: Vec<TestCase> = serde_json::from_reader(f).unwrap();

            for test_case in test_cases {
                let default_assignment =
                    AssignmentValue::from_wire(test_case.variation_type, test_case.default_value)
                        .unwrap();

                print!("test subject {:?} ... ", test_case.targeting_key);
                let result = get_assignment(
                    Some(&config),
                    &test_case.flag,
                    &test_case.targeting_key,
                    &test_case.attributes,
                    Some(test_case.variation_type),
                    now,
                )
                .unwrap_or(None);

                let result_assingment = result
                    .as_ref()
                    .map(|assignment| &assignment.value)
                    .unwrap_or(&default_assignment);
                let expected_assignment =
                    AssignmentValue::from_wire(test_case.variation_type, test_case.result.value)
                        .unwrap();

                assert_eq!(result_assingment, &expected_assignment);
                println!("ok");
            }
        }
    }
}
