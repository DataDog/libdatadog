// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use chrono::{DateTime, Utc};

use crate::rules_based::{
    Configuration, EvaluationContext, ExpectedFlagType, Timestamp,
    error::EvaluationError,
    timestamp::OptOutTimestamp,
    ufc::{Allocation, Assignment, AssignmentReason, CompiledFlagsConfig, Flag, Shard, Split},
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
        let exclusion = options.experimentation_exclusion(context)?;
        let result =
            self.flags
                .compiled
                .eval_flag(flag_key, context, expected_type, now, exclusion);

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
        exclusion: ExperimentationExclusion,
    ) -> Result<Assignment, EvaluationError> {
        self.get_flag(flag_key)?
            .eval(subject, expected_type, now, exclusion)
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
        exclusion: ExperimentationExclusion,
    ) -> Result<Assignment, EvaluationError> {
        // D004: flag-wide activation validity is consumed only by effective future policy.
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

        let (allocation, split, reason) = self.find_allocation(context, now, exclusion)?;

        Ok(Assignment {
            value: split.value.clone(),
            variation_key: split.variation_key.clone(),
            allocation_key: allocation.key.clone(),
            reason,
            serial_id: split.serial_id,
            do_log: allocation.do_log,
            features: split.features.clone(),
        })
    }

    fn find_allocation(
        &self,
        context: &EvaluationContext,
        now: DateTime<Utc>,
        exclusion: ExperimentationExclusion,
    ) -> Result<(&Allocation, &Split, AssignmentReason), EvaluationError> {
        for allocation in &self.allocations {
            if exclusion != ExperimentationExclusion::None && exclusion.excludes(allocation) {
                continue;
            }
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
        let has_time_bounds = self.start_at.is_some() || self.end_at.is_some();
        let reason = if !self.rules.is_empty() {
            AssignmentReason::TargetingMatch
        } else if has_time_bounds && self.splits.len() == 1 && split.shards.is_empty() {
            AssignmentReason::Default
        } else if self.splits.len() == 1 && !self.splits[0].has_shards {
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
    use crate::rules_based::{
        AssignmentReason, AssignmentValue, FeatureValue, UniversalFlagConfig,
    };

    #[test]
    fn assignment_carries_features_of_selected_split() {
        let config = UniversalFlagConfig::from_json(
            br#"
            {
              "createdAt": "2026-08-11T00:00:00Z",
              "environment": {"name": "test"},
              "flags": {
                "with-features": {
                  "key": "with-features",
                  "enabled": true,
                  "variationType": "STRING",
                  "variations": {"on": {"key": "on", "value": "on"}},
                  "allocations": [{
                    "key": "static",
                    "splits": [{
                      "variationKey": "on",
                      "shards": [],
                      "serialId": 7,
                      "features": [
                        {"key": "holdout.key", "value": "abc", "destinations": ["HOOK"]},
                        {"key": "holdout.fraction", "value": 0.5, "destinations": ["EXPOSURE"]},
                        {"key": "holdout.enabled", "value": false, "destinations": ["HOOK"]}
                      ]
                    }]
                  }]
                },
                "without-features": {
                  "key": "without-features",
                  "enabled": true,
                  "variationType": "STRING",
                  "variations": {"on": {"key": "on", "value": "on"}},
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

        let assignment = config
            .eval_flag("with-features", &context, ExpectedFlagType::String, now)
            .unwrap();
        let features = assignment.features_slice();
        assert_eq!(features.len(), 3);
        assert_eq!(features[0].key.as_str(), "holdout.key");
        assert_eq!(features[0].value, FeatureValue::String("abc".into()));
        assert_eq!(features[0].destinations.len(), 1);
        assert_eq!(features[0].destinations[0].as_str(), "HOOK");
        assert_eq!(features[1].value, FeatureValue::Number(0.5));
        assert_eq!(features[2].value, FeatureValue::Boolean(false));
        assert_eq!(assignment.serial_id, Some(7));

        let assignment = config
            .eval_flag("without-features", &context, ExpectedFlagType::String, now)
            .unwrap();
        assert!(assignment.features.is_none());
        assert!(assignment.features_slice().is_empty());
    }

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
