// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};

use crate::rules_based::{
    Configuration, EvaluationContext, ExpectedFlagType, Str, Timestamp,
    error::EvaluationError,
    timestamp::OptOutTimestamp,
    ufc::{
        Allocation, Assignment, AssignmentReason, CompiledFlagsConfig, ConditionCheck, Flag,
        RuleWire, Shard, Split,
    },
};

/// Selects how a subject's opt-out timestamp affects exposure-logging allocations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExperimentationOptOutMode {
    /// Ignore the opt-out attribute.
    #[default]
    Disabled,
    /// Exclude all exposure-logging allocations immediately.
    Immediate,
    /// Exclude allocations activated at or after opt-out, or with unknown activation.
    FutureExperimentsOnly,
}

/// Controls optional behavior for one evaluation. The SDK resolves configuration.
#[derive(Clone, Copy, Debug, Default)]
pub struct EvaluationOptions {
    /// Honor the subject's `dd_experimentation_opt_out_at` timestamp.
    pub experimentation_opt_out_mode: ExperimentationOptOutMode,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ExperimentationExclusion {
    #[default]
    None,
    Immediate,
    ActivatedAtOrAfter(Timestamp),
}

#[cfg(test)]
thread_local! {
    static EXCLUSION_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl ExperimentationExclusion {
    fn excludes(self, allocation: &Allocation) -> bool {
        #[cfg(test)]
        EXCLUSION_CALLS.with(|calls| calls.set(calls.get() + 1));
        if !allocation.do_log {
            return false;
        }
        match self {
            Self::None => false,
            Self::Immediate => true,
            Self::ActivatedAtOrAfter(cutoff) => allocation
                .randomization_activated_at
                .is_none_or(|activation| activation >= cutoff),
        }
    }
}

impl EvaluationOptions {
    fn experimentation_exclusion(
        self,
        context: &EvaluationContext,
    ) -> Result<ExperimentationExclusion, EvaluationError> {
        if self.experimentation_opt_out_mode == ExperimentationOptOutMode::Disabled {
            return Ok(ExperimentationExclusion::None);
        }
        let Some(attribute) = context.get_attribute("dd_experimentation_opt_out_at") else {
            return Ok(ExperimentationExclusion::None);
        };
        let Some(timestamp) = attribute.as_str() else {
            return Err(EvaluationError::InvalidContext);
        };
        let cutoff = OptOutTimestamp::parse(timestamp)
            .map_err(|_| EvaluationError::InvalidContext)?
            .0;
        Ok(match self.experimentation_opt_out_mode {
            ExperimentationOptOutMode::Disabled => ExperimentationExclusion::None,
            ExperimentationOptOutMode::Immediate => ExperimentationExclusion::Immediate,
            ExperimentationOptOutMode::FutureExperimentsOnly => {
                ExperimentationExclusion::ActivatedAtOrAfter(cutoff)
            }
        })
    }
}

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
        self.eval_flag_with_options(
            flag_key,
            context,
            expected_type,
            now,
            EvaluationOptions::default(),
        )
    }

    /// Evaluate a flag with SDK-selected behavior. No environment is read here.
    pub fn eval_flag_with_options(
        &self,
        flag_key: &str,
        context: &EvaluationContext,
        expected_type: ExpectedFlagType,
        now: DateTime<Utc>,
        options: EvaluationOptions,
    ) -> Result<Assignment, EvaluationError> {
        let result = self
            .eval_flag_with_trace(flag_key, context, expected_type, now, options)
            .result;

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

    /// Evaluate with the prerequisite records needed for SDK evaluation and exposure logging.
    /// Records exclude the requested root flag and occur once per evaluated prerequisite.
    /// An unsuccessful root has no eligible prerequisite exposures. SDKs must additionally
    /// honor consent and exposure deduplication policy.
    pub fn eval_flag_with_trace(
        &self,
        flag_key: &str,
        context: &EvaluationContext,
        expected_type: ExpectedFlagType,
        now: DateTime<Utc>,
        options: EvaluationOptions,
    ) -> EvaluationResult {
        let exclusion = match options.experimentation_exclusion(context) {
            Ok(exclusion) => exclusion,
            Err(error) => {
                return EvaluationResult {
                    result: Err(error),
                    evaluations: Vec::new(),
                };
            }
        };
        // Most evaluations have no dependencies. Preserve their allocation-only path rather
        // than initializing memoization and exposure bookkeeping for every SDK call.
        match self.flags.compiled.get_flag(flag_key) {
            Ok(flag) if !flag.has_dependencies => {
                return EvaluationResult {
                    result: flag.evaluate_independent(context, expected_type, now, exclusion),
                    evaluations: Vec::new(),
                };
            }
            Err(error) => {
                return EvaluationResult {
                    result: Err(error),
                    evaluations: Vec::new(),
                };
            }
            Ok(_) => {}
        }
        let mut session = EvaluationSession {
            config: &self.flags.compiled,
            root_key: flag_key,
            context,
            now,
            exclusion,
            active: Vec::new(),
            memo: HashMap::new(),
            evaluations: Vec::new(),
        };
        let evaluated = session.evaluate_flag(flag_key, expected_type, 0);
        if let Ok(root) = &evaluated {
            for record in &mut session.evaluations {
                record.exposure_eligible = root.exposures.contains(&record.flag_key)
                    && matches!(record.result.as_ref(), Ok(assignment) if assignment.do_log);
            }
        }
        EvaluationResult {
            result: evaluated.map(|result| result.assignment),
            evaluations: session.evaluations,
        }
    }
}

/// One requested evaluation and the prerequisites it actually evaluated.
#[derive(Debug)]
pub struct EvaluationResult {
    /// The requested flag's result; SDKs return their caller default on failure.
    pub result: Result<Assignment, EvaluationError>,
    /// Unique prerequisite evaluations; excludes the root. Record order is unspecified.
    pub evaluations: Vec<EvaluationRecord>,
}

/// Telemetry for a prerequisite reached within one root evaluation.
#[derive(Debug)]
pub struct EvaluationRecord {
    /// Key of the evaluated prerequisite.
    pub flag_key: Str,
    /// The prerequisite result, including failures.
    pub result: Result<Assignment, EvaluationError>,
    /// Whether this result belongs to the root's successful assignment path and its selected
    /// assignment permits exposure logging. SDKs still apply their ordinary exposure policy.
    pub exposure_eligible: bool,
}

#[derive(Clone)]
struct EvaluatedFlag {
    assignment: Assignment,
    // Includes all reached branches, even rejected rules, to keep memoization depth-safe.
    height: u16,
    // Only dependencies on the selected rule/split path, not every successful evaluation.
    exposures: HashSet<Str>,
}

struct EvaluationSession<'a> {
    config: &'a CompiledFlagsConfig,
    root_key: &'a str,
    context: &'a EvaluationContext,
    now: Timestamp,
    exclusion: ExperimentationExclusion,
    active: Vec<Str>,
    memo: HashMap<Str, EvaluatedFlag>,
    evaluations: Vec<EvaluationRecord>,
}

impl CompiledFlagsConfig {
    fn get_flag(&self, flag_key: &str) -> Result<&Flag, EvaluationError> {
        self.flags
            .get(flag_key)
            .ok_or(EvaluationError::FlagUnrecognizedOrDisabled)?
            .as_ref()
            .map_err(Clone::clone)
    }
}

impl Flag {
    fn validate(
        &self,
        expected_type: ExpectedFlagType,
        exclusion: ExperimentationExclusion,
    ) -> Result<(), EvaluationError> {
        if matches!(exclusion, ExperimentationExclusion::ActivatedAtOrAfter(_))
            && self.has_invalid_activation
        {
            return Err(EvaluationError::FlagConfigurationInvalid);
        }
        if !expected_type.is_compatible(self.variation_type.into()) {
            return Err(EvaluationError::TypeMismatch {
                expected: expected_type,
                found: self.variation_type.into(),
            });
        }
        Ok(())
    }

    fn evaluate_independent(
        &self,
        context: &EvaluationContext,
        expected_type: ExpectedFlagType,
        now: Timestamp,
        exclusion: ExperimentationExclusion,
    ) -> Result<Assignment, EvaluationError> {
        self.validate(expected_type, exclusion)?;
        for allocation in &self.allocations {
            if !allocation.is_active(now, exclusion) {
                continue;
            }
            if !allocation.rules.is_empty()
                && !allocation.rules.iter().any(|rule| rule.eval(context))
            {
                continue;
            }
            if let Some(split) = allocation.find_split(context)? {
                return Ok(allocation.assignment(split));
            }
        }
        Err(EvaluationError::DefaultAllocationNull)
    }
}

impl EvaluationSession<'_> {
    fn evaluate_flag(
        &mut self,
        flag_key: &str,
        expected_type: ExpectedFlagType,
        depth: u16,
    ) -> Result<EvaluatedFlag, EvaluationError> {
        let flag = self.config.get_flag(flag_key)?;
        flag.validate(expected_type, self.exclusion)?;
        if depth > 0 {
            self.active.push(flag_key.into());
        }
        let result = self.evaluate_allocations(&flag.allocations, depth);
        if depth > 0 {
            self.active.pop();
        }
        result
    }

    fn evaluate_allocations(
        &mut self,
        allocations: &[Allocation],
        depth: u16,
    ) -> Result<EvaluatedFlag, EvaluationError> {
        let mut height = 0;
        for allocation in allocations {
            if !allocation.is_active(self.now, self.exclusion) {
                continue;
            }
            let mut selected_exposures = None;
            if allocation.rules.is_empty() {
                selected_exposures = Some(HashSet::new());
            } else {
                for rule in &allocation.rules {
                    if let Some(exposures) = self.evaluate_rule(rule, depth, &mut height)? {
                        selected_exposures = Some(exposures);
                        break;
                    }
                }
            }
            let Some(exposures) = selected_exposures else {
                continue;
            };
            let Some(split) = allocation.find_split(self.context)? else {
                continue;
            };
            return Ok(EvaluatedFlag {
                assignment: allocation.assignment(split),
                height,
                exposures,
            });
        }
        Err(EvaluationError::DefaultAllocationNull)
    }

    fn evaluate_rule(
        &mut self,
        rule: &RuleWire,
        depth: u16,
        height: &mut u16,
    ) -> Result<Option<HashSet<Str>>, EvaluationError> {
        let mut exposures = HashSet::new();
        for condition in &rule.conditions {
            let matched = if let ConditionCheck::FlagEvaluation {
                key,
                expected_membership,
                values,
            } = &condition.check
            {
                let dependency = self.evaluate_dependency(key, depth + 1).map_err(|source| {
                    EvaluationError::DependencyEvaluationFailed {
                        flag_key: key.clone(),
                        source: Box::new(source),
                    }
                })?;
                *height = (*height).max(1 + dependency.height);
                let matched = values
                    .iter()
                    .any(|value| value.as_ref() == dependency.assignment.variation_key.as_str())
                    == *expected_membership;
                if matched {
                    exposures.insert(key.clone());
                    exposures.extend(dependency.exposures);
                }
                matched
            } else {
                condition.eval(self.context)
            };
            if !matched {
                return Ok(None);
            }
        }
        Ok(Some(exposures))
    }

    fn evaluate_dependency(
        &mut self,
        key: &Str,
        depth: u16,
    ) -> Result<EvaluatedFlag, EvaluationError> {
        let limit = u16::from(self.config.max_dependency_depth);
        if depth > limit {
            let error = EvaluationError::DependencyDepthExceeded;
            if key.as_str() != self.root_key
                && !self.active.contains(key)
                && !self.memo.contains_key(key)
            {
                self.evaluations.push(EvaluationRecord {
                    flag_key: key.clone(),
                    result: Err(error.clone()),
                    exposure_eligible: false,
                });
            }
            return Err(error);
        }
        if key.as_str() == self.root_key || self.active.contains(key) {
            return Err(EvaluationError::DependencyCycle);
        }
        if let Some(result) = self.memo.get(key) {
            if depth + result.height > limit {
                return Err(EvaluationError::DependencyDepthExceeded);
            }
            return Ok(result.clone());
        }
        let result = self
            .evaluate_flag(key, ExpectedFlagType::Any, depth)
            .and_then(|result| {
                if result.assignment.variation_key.is_empty() {
                    Err(EvaluationError::DefaultAllocationNull)
                } else {
                    Ok(result)
                }
            });
        self.evaluations.push(EvaluationRecord {
            flag_key: key.clone(),
            result: result
                .as_ref()
                .map(|result| result.assignment.clone())
                .map_err(Clone::clone),
            exposure_eligible: false,
        });
        if let Ok(result) = &result {
            self.memo.insert(key.clone(), result.clone());
        }
        result
    }
}

impl Allocation {
    fn is_active(&self, now: Timestamp, exclusion: ExperimentationExclusion) -> bool {
        !(exclusion != ExperimentationExclusion::None && exclusion.excludes(self)
            || self.start_at.is_some_and(|t| now < t)
            || self.end_at.is_some_and(|t| now > t))
    }

    fn assignment(&self, split: &Split) -> Assignment {
        Assignment {
            value: split.value.clone(),
            variation_key: split.variation_key.clone(),
            allocation_key: self.key.clone(),
            reason: self.assignment_reason(split),
            serial_id: split.serial_id,
            do_log: self.do_log,
        }
    }

    fn assignment_reason(&self, split: &Split) -> AssignmentReason {
        let has_time_bounds = self.start_at.is_some() || self.end_at.is_some();
        if !self.rules.is_empty() {
            AssignmentReason::TargetingMatch
        } else if has_time_bounds && self.splits.len() == 1 && split.shards.is_empty() {
            AssignmentReason::Default
        } else if self.splits.len() == 1 && !self.splits[0].has_shards {
            AssignmentReason::Static
        } else {
            AssignmentReason::Split
        }
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
mod opt_out_tests {
    use super::*;
    use crate::rules_based::{Attribute, Str, UniversalFlagConfig};
    use serde_json::json;
    use std::{collections::HashMap, sync::Arc};

    fn context(value: Option<serde_json::Value>) -> EvaluationContext {
        let mut attributes = HashMap::<Str, Attribute>::new();
        if let Some(value) = value {
            attributes.insert(
                "dd_experimentation_opt_out_at".into(),
                serde_json::from_value(value).unwrap(),
            );
        }
        EvaluationContext::new(None, Arc::new(attributes))
    }

    #[test]
    fn opt_out_timestamp_validation_and_disabled_mode() {
        let immediate = EvaluationOptions {
            experimentation_opt_out_mode: ExperimentationOptOutMode::Immediate,
        };
        for (value, expected) in [
            (None, Ok(false)),
            (Some(json!("2026-10-19T12:00:00Z")), Ok(true)),
            (Some(json!("2099-01-01T00:00:00.123456789+05:30")), Ok(true)),
            (Some(json!(null)), Err(EvaluationError::InvalidContext)),
            (Some(json!(true)), Err(EvaluationError::InvalidContext)),
            (Some(json!(123)), Err(EvaluationError::InvalidContext)),
            (Some(json!("")), Err(EvaluationError::InvalidContext)),
            (
                Some(json!("2026-02-30T00:00:00Z")),
                Err(EvaluationError::InvalidContext),
            ),
            (
                Some(json!("2026-10-19T12:00:00")),
                Err(EvaluationError::InvalidContext),
            ),
        ] {
            let subject = context(value.clone());
            assert_eq!(
                immediate
                    .experimentation_exclusion(&subject)
                    .map(|value| value != ExperimentationExclusion::None),
                expected,
                "{value:?}"
            );
            assert_eq!(
                EvaluationOptions::default().experimentation_exclusion(&subject),
                Ok(ExperimentationExclusion::None)
            );
        }
    }

    #[test]
    fn opt_out_timestamp_syntax_is_explicit_and_errors_are_redacted() {
        let immediate = EvaluationOptions {
            experimentation_opt_out_mode: ExperimentationOptOutMode::Immediate,
        };
        for (timestamp, accepted) in [
            ("2026-10-19T12:00:00Z", true),
            ("2099-01-01T00:00:00.123456789+05:30", true),
            ("2026-10-19 12:00:00Z", false),
            ("2026-10-19t12:00:00z", true),
            ("2016-12-31T23:59:60Z", false),
            ("2026-10-19T1:00:00Z", false),
            ("2026-10-19T12:00:00,123Z", false),
            ("2026-10-19T12:00:00+24:00", false),
            ("2026-02-30T00:00:00Z", false),
            ("2026-10-19T12:00:00", false),
            ("private-invalid-timestamp", false),
            ("", false),
        ] {
            let result = immediate.experimentation_exclusion(&context(Some(json!(timestamp))));
            let expected = if accepted {
                Ok(ExperimentationExclusion::Immediate)
            } else {
                Err(EvaluationError::InvalidContext)
            };
            assert_eq!(result, expected, "{timestamp:?}");
            if let Err(error) = result {
                assert!(!error.to_string().contains(timestamp) || timestamp.is_empty());
            }
        }
    }

    #[test]
    fn opt_out_skips_before_targeting_and_preserves_ordinary_fallback() {
        let config = Configuration::from_server_response(
            UniversalFlagConfig::from_json(serde_json::to_vec(&json!({
                "createdAt": "2026-10-19T00:00:00Z",
                "environment": {"name": "test"},
                "flags": {"feature": {
                    "key": "feature",
                    "enabled": true,
                    "variationType": "BOOLEAN",
                    "variations": {
                        "on": {"key": "on", "value": true},
                        "off": {"key": "off", "value": false}
                    },
                    "allocations": [{
                        "key": "experiment",
                        "splits": [{"variationKey": "on", "shards": [{
                            "salt": "test", "totalShards": 10000, "ranges": [{"start": 0, "end": 5000}]
                        }]}]
                    }, {
                        "key": "ordinary", "doLog": false,
                        "splits": [{"variationKey": "off", "shards": []}]
                    }]
                }}
            })).unwrap()).unwrap()
        );
        let subject = context(Some(json!("2026-10-19T00:00:00Z")));
        // Missing targeting key would fail in the first allocation if it ran.
        assert_eq!(
            config
                .eval_flag("feature", &subject, ExpectedFlagType::Boolean, Utc::now())
                .unwrap_err(),
            EvaluationError::TargetingKeyMissing,
        );
        let assignment = config
            .eval_flag_with_options(
                "feature",
                &subject,
                ExpectedFlagType::Boolean,
                Utc::now(),
                EvaluationOptions {
                    experimentation_opt_out_mode: ExperimentationOptOutMode::Immediate,
                },
            )
            .unwrap();
        assert_eq!(assignment.allocation_key.as_str(), "ordinary");
        assert_eq!(assignment.value.variation_value(), json!(false));
        assert!(!assignment.do_log);
    }

    fn future_configuration(activation: serde_json::Value) -> Configuration {
        let mut flag = json!({
            "key": "feature", "enabled": true, "variationType": "BOOLEAN",
            "variations": {
                "on": {"key": "on", "value": true},
                "off": {"key": "off", "value": false}
            },
            "allocations": [{
                "key": "experiment", "doLog": true,
                "randomizationActivatedAt": activation,
                "splits": [{"variationKey": "on", "shards": [], "serialId": 7}]
            }, {
                "key": "ordinary", "doLog": false,
                "splits": [{"variationKey": "off", "shards": [], "serialId": 8}]
            }]
        });
        let original = flag.clone();
        flag["key"] = json!("neighbor");
        flag["allocations"][0]["randomizationActivatedAt"] = serde_json::Value::Null;
        Configuration::from_server_response(
            UniversalFlagConfig::from_json(
                serde_json::to_vec(&json!({
                    "createdAt": "2026-10-19T00:00:00Z",
                    "environment": {"name": "test"},
                    "flags": {"feature": original, "neighbor": flag}
                }))
                .unwrap(),
            )
            .unwrap(),
        )
    }

    #[test]
    fn future_cutoff_preserves_precision_and_exposure_selection() {
        let subject = context(Some(json!("2026-10-19T12:00:00Z")));
        for (activation, eligible) in [
            (json!("2026-10-19T11:59:59.999999999Z"), true),
            (json!("2026-10-19T12:00:00Z"), false),
            (json!("2026-10-19T12:00:00.000000001Z"), false),
            (json!("2026-10-19T17:30:00+05:30"), false),
            (serde_json::Value::Null, false),
        ] {
            let config = future_configuration(activation.clone());
            for mode in [
                ExperimentationOptOutMode::Disabled,
                ExperimentationOptOutMode::Immediate,
                ExperimentationOptOutMode::FutureExperimentsOnly,
            ] {
                let options = EvaluationOptions {
                    experimentation_opt_out_mode: mode,
                };
                let expected = mode == ExperimentationOptOutMode::Disabled
                    || (mode == ExperimentationOptOutMode::FutureExperimentsOnly && eligible);
                let result = config
                    .eval_flag_with_options(
                        "feature",
                        &subject,
                        ExpectedFlagType::Boolean,
                        Utc::now(),
                        options,
                    )
                    .unwrap();
                assert_eq!(
                    result.value.variation_value(),
                    json!(expected),
                    "{mode:?}: {activation}"
                );
                assert_eq!(result.do_log, expected);
                assert_eq!(
                    result.allocation_key.as_str(),
                    if expected { "experiment" } else { "ordinary" }
                );
                assert_eq!(result.serial_id, Some(if expected { 7 } else { 8 }));
                let eligible_subject = config
                    .eval_flag_with_options(
                        "feature",
                        &context(None),
                        ExpectedFlagType::Boolean,
                        Utc::now(),
                        options,
                    )
                    .unwrap();
                assert!(eligible_subject.do_log);
            }
        }
    }

    #[test]
    fn inactive_policy_does_not_call_allocation_exclusion() {
        // D017: observe the production boundary, not a count inferred from the policy.
        let config = future_configuration(json!("2026-10-19T12:00:00Z"));
        for mode in [
            ExperimentationOptOutMode::Disabled,
            ExperimentationOptOutMode::Immediate,
            ExperimentationOptOutMode::FutureExperimentsOnly,
        ] {
            for attribute in [None, Some(json!(true)), Some(json!("2026-10-19T12:00:00Z"))] {
                EXCLUSION_CALLS.with(|calls| calls.set(0));
                let result = config.eval_flag_with_options(
                    "feature",
                    &context(attribute.clone()),
                    ExpectedFlagType::Boolean,
                    Utc::now(),
                    EvaluationOptions {
                        experimentation_opt_out_mode: mode,
                    },
                );
                let calls = EXCLUSION_CALLS.with(|calls| calls.get());
                if mode == ExperimentationOptOutMode::Disabled || attribute.is_none() {
                    assert_eq!(calls, 0);
                    assert!(result.unwrap().do_log);
                } else if attribute == Some(json!(true)) {
                    assert_eq!(calls, 0);
                    assert_eq!(result.unwrap_err(), EvaluationError::InvalidContext);
                } else {
                    assert_eq!(calls, 2);
                    assert!(!result.unwrap().do_log);
                }
            }
        }
    }

    #[test]
    fn malformed_activation_is_scoped_to_effective_future_mode() {
        for activation in [json!("bad"), json!(42), json!(true), json!([]), json!({})] {
            let config = future_configuration(activation);
            for mode in [
                ExperimentationOptOutMode::Disabled,
                ExperimentationOptOutMode::Immediate,
                ExperimentationOptOutMode::FutureExperimentsOnly,
            ] {
                let options = EvaluationOptions {
                    experimentation_opt_out_mode: mode,
                };
                for cutoff in [None, Some(json!("2026-10-19T12:00:00Z"))] {
                    let subject = context(cutoff.clone());
                    let result = config.eval_flag_with_options(
                        "feature",
                        &subject,
                        ExpectedFlagType::Boolean,
                        Utc::now(),
                        options,
                    );
                    if mode == ExperimentationOptOutMode::FutureExperimentsOnly && cutoff.is_some()
                    {
                        assert_eq!(
                            result.unwrap_err(),
                            EvaluationError::FlagConfigurationInvalid
                        );
                    } else {
                        let assignment = result.unwrap();
                        assert_eq!(
                            assignment.do_log,
                            mode != ExperimentationOptOutMode::Immediate || cutoff.is_none()
                        );
                    }
                    assert!(
                        config
                            .eval_flag_with_options(
                                "neighbor",
                                &subject,
                                ExpectedFlagType::Boolean,
                                Utc::now(),
                                options,
                            )
                            .is_ok()
                    );
                }
            }
        }
    }

    #[test]
    fn future_invalid_context_precedes_flag_lookup() {
        let config = future_configuration(json!("2026-10-19T00:00:00Z"));
        for value in [
            json!(null),
            json!(true),
            json!(42),
            json!(""),
            json!("private-invalid"),
        ] {
            let result = config.eval_flag_with_options(
                "missing",
                &context(Some(value)),
                ExpectedFlagType::Boolean,
                Utc::now(),
                EvaluationOptions {
                    experimentation_opt_out_mode: ExperimentationOptOutMode::FutureExperimentsOnly,
                },
            );
            assert_eq!(result.unwrap_err(), EvaluationError::InvalidContext);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use super::*;
    use crate::rules_based::{AssignmentReason, AssignmentValue, UniversalFlagConfig};

    #[test]
    fn invalid_flag_does_not_poison_valid_neighbor() {
        let config = UniversalFlagConfig::from_json(
            br#"
            {
              "createdAt": "2026-08-11T00:00:00Z",
              "environment": {"name": "test"},
              "flags": {
                "invalid-regex": {
                  "key": "invalid-regex",
                  "enabled": true,
                  "variationType": "STRING",
                  "variations": {"trap": {"key": "trap", "value": "trap"}},
                  "allocations": [{
                    "key": "invalid",
                    "rules": [{"conditions": [{
                      "attribute": "email",
                      "operator": "MATCHES",
                      "value": "[invalid"
                    }]}],
                    "splits": [{"variationKey": "trap", "shards": []}]
                  }]
                },
                "valid": {
                  "key": "valid",
                  "enabled": true,
                  "variationType": "STRING",
                  "variations": {"on": {"key": "on", "value": "valid"}},
                  "allocations": [{
                    "key": "static",
                    "splits": [{"variationKey": "on", "shards": []}]
                  }]
                }
              }
            }
            "#
            .to_vec(),
        )
        .unwrap();
        let config = Configuration::from_server_response(config);
        let context = EvaluationContext::new(None, Arc::new(HashMap::new()));
        let now = chrono::Utc::now();

        assert_eq!(
            config
                .eval_flag("invalid-regex", &context, ExpectedFlagType::String, now)
                .unwrap_err(),
            EvaluationError::FlagConfigurationInvalid
        );

        let assignment = config
            .eval_flag("valid", &context, ExpectedFlagType::String, now)
            .unwrap();
        assert!(matches!(
            assignment.value,
            AssignmentValue::String(ref value) if value.as_str() == "valid"
        ));
        assert_eq!(assignment.reason, AssignmentReason::Static);
    }
}
