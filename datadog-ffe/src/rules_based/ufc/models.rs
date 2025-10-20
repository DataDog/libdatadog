use std::{collections::HashMap, sync::Arc};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::rules_based::{Error, EvaluationError, Str};

#[allow(missing_docs)]
pub type Timestamp = crate::rules_based::timestamp::Timestamp;

// Temporary workaround till we figure out one proper format
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum WireTimestamp {
    Iso8601(Timestamp),
    UnixMs(i64),
}

impl From<WireTimestamp> for Timestamp {
    fn from(value: WireTimestamp) -> Self {
        match value {
            WireTimestamp::Iso8601(ts) => ts,
            WireTimestamp::UnixMs(unix) => {
                Timestamp::from_timestamp_millis(unix).expect("timestamp should be in range")
            }
        }
    }
}

/// JSON API wrapper for Universal Flag Configuration.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct UniversalFlagConfigWire {
    /// JSON API data envelope.
    pub data: UniversalFlagConfigData,
}

/// JSON API data structure for Universal Flag Configuration.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct UniversalFlagConfigData {
    /// JSON API type field.
    #[serde(rename = "type")]
    pub data_type: String,
    /// JSON API id field.
    pub id: String,
    /// JSON API attributes containing the actual UFC data.
    pub attributes: UniversalFlagConfigAttributes,
}

/// Universal Flag Configuration attributes. This contains the actual flag configuration data.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UniversalFlagConfigAttributes {
    /// When configuration was last updated.
    pub created_at: WireTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<ConfigurationFormat>,
    /// Environment this configuration belongs to.
    pub environment: Environment,
    /// Flags configuration.
    ///
    /// Value is wrapped in `TryParse` so that if we fail to parse one flag (e.g., new server
    /// format), we can still serve other flags.
    pub flags: HashMap<Str, TryParse<FlagWire>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConfigurationFormat {
    Client,
    Server,
    Precomputed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    /// Name of the environment.
    pub name: Str,
}

/// `TryParse` allows the subfield to fail parsing without failing the parsing of the whole
/// structure.
///
/// This can be helpful to isolate errors in a subtree. e.g., if configuration for one flag parses,
/// the rest of the flags are still usable.
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum TryParse<T> {
    /// Successfully parsed.
    Parsed(T),
    /// Parsing failed.
    ParseFailed(serde_json::Value),
}
impl<T> From<T> for TryParse<T> {
    fn from(value: T) -> TryParse<T> {
        TryParse::Parsed(value)
    }
}
impl<T> From<TryParse<T>> for Option<T> {
    fn from(value: TryParse<T>) -> Self {
        match value {
            TryParse::Parsed(v) => Some(v),
            TryParse::ParseFailed(_) => None,
        }
    }
}
impl<'a, T> From<&'a TryParse<T>> for Option<&'a T> {
    fn from(value: &TryParse<T>) -> Option<&T> {
        match value {
            TryParse::Parsed(v) => Some(v),
            TryParse::ParseFailed(_) => None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub(crate) struct FlagWire {
    pub key: Str,
    pub enabled: bool,
    pub variation_type: VariationType,
    pub variations: HashMap<String, VariationWire>,
    pub allocations: Vec<AllocationWire>,
}

/// Type of the variation.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[allow(missing_docs)]
pub enum VariationType {
    String,
    Integer,
    Numeric,
    Boolean,
    Json,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub(crate) struct VariationWire {
    pub key: Str,
    pub value: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub(crate) struct AllocationWire {
    pub key: Str,
    #[serde(default)]
    pub rules: Option<Box<[RuleWire]>>,
    #[serde(default)]
    pub start_at: Option<Timestamp>,
    #[serde(default)]
    pub end_at: Option<Timestamp>,
    pub splits: Vec<SplitWire>,
    #[serde(default = "default_do_log")]
    pub do_log: bool,
}

fn default_do_log() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub(crate) struct RuleWire {
    pub conditions: Vec<TryParse<Condition>>,
}

/// `Condition` is a check that given user `attribute` matches the condition `value` under the given
/// `operator`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "ConditionWire", into = "ConditionWire")]
pub(crate) struct Condition {
    pub attribute: Box<str>,
    pub check: ConditionCheck,
}

#[derive(Debug, Clone)]
pub(crate) enum ConditionCheck {
    Comparison {
        operator: ComparisonOperator,
        comparand: f64,
    },
    Regex {
        expected_match: bool,
        // As regex is supplied by user, we allow regex parse failure to not fail parsing and
        // evaluation. Invalid regexes are simply ignored.
        regex: Regex,
    },
    Membership {
        expected_membership: bool,
        values: Box<[Box<str>]>,
    },
    Null {
        expected_null: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum ComparisonOperator {
    Gte,
    Gt,
    Lte,
    Lt,
}

impl From<ComparisonOperator> for ConditionOperator {
    fn from(value: ComparisonOperator) -> ConditionOperator {
        match value {
            ComparisonOperator::Gte => ConditionOperator::Gte,
            ComparisonOperator::Gt => ConditionOperator::Gt,
            ComparisonOperator::Lte => ConditionOperator::Lte,
            ComparisonOperator::Lt => ConditionOperator::Lt,
        }
    }
}

/// Wire (JSON) format for the `Condition`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub(crate) struct ConditionWire {
    pub attribute: Box<str>,
    pub operator: ConditionOperator,
    pub value: ConditionValue,
}

impl From<Condition> for ConditionWire {
    fn from(condition: Condition) -> Self {
        let (operator, value) = match condition.check {
            ConditionCheck::Comparison {
                operator,
                comparand,
            } => (operator.into(), comparand.into()),
            ConditionCheck::Regex {
                expected_match,
                regex,
            } => (
                if expected_match {
                    ConditionOperator::Matches
                } else {
                    ConditionOperator::NotMatches
                },
                ConditionValue::Single(SingleConditionValue::String(Str::from(regex.as_str()))),
            ),
            ConditionCheck::Membership {
                expected_membership,
                values,
            } => (
                if expected_membership {
                    ConditionOperator::OneOf
                } else {
                    ConditionOperator::NotOneOf
                },
                ConditionValue::Multiple(values),
            ),
            ConditionCheck::Null { expected_null } => {
                (ConditionOperator::IsNull, expected_null.into())
            }
        };
        ConditionWire {
            attribute: condition.attribute,
            operator,
            value,
        }
    }
}

impl From<ConditionWire> for Option<Condition> {
    fn from(value: ConditionWire) -> Self {
        Condition::try_from(value).ok()
    }
}

impl TryFrom<ConditionWire> for Condition {
    type Error = Error;

    fn try_from(condition: ConditionWire) -> Result<Self, Self::Error> {
        let attribute = condition.attribute;
        let check = match condition.operator {
            ConditionOperator::Matches | ConditionOperator::NotMatches => {
                let expected_match = condition.operator == ConditionOperator::Matches;

                let regex_string = match condition.value.singleton() {
                    Some(SingleConditionValue::String(s)) => s,
                    _ => {
                        log::warn!(
                            "failed to parse condition: {:?} condition with non-string condition value",
                            condition.operator
                        );
                        return Err(Error::EvaluationError(
                            EvaluationError::UnexpectedConfigurationParseError,
                        ));
                    }
                };
                let regex = match Regex::new(&regex_string) {
                    Ok(regex) => regex,
                    Err(err) => {
                        log::warn!(
                            "failed to parse condition: failed to compile regex {regex_string:?}: {err:?}"
                        );
                        return Err(Error::EvaluationError(
                            EvaluationError::UnexpectedConfigurationParseError,
                        ));
                    }
                };

                ConditionCheck::Regex {
                    expected_match,
                    regex,
                }
            }
            ConditionOperator::Gte
            | ConditionOperator::Gt
            | ConditionOperator::Lte
            | ConditionOperator::Lt => {
                let operator = match condition.operator {
                    ConditionOperator::Gte => ComparisonOperator::Gte,
                    ConditionOperator::Gt => ComparisonOperator::Gt,
                    ConditionOperator::Lte => ComparisonOperator::Lte,
                    ConditionOperator::Lt => ComparisonOperator::Lt,
                    _ => unreachable!(),
                };

                // numeric comparison only
                let Some(condition_value) = condition.value.singleton().and_then(|v| v.as_number())
                else {
                    log::warn!(
                        "failed to parse condition: comparison value is not a number: {:?}",
                        condition.value
                    );
                    return Err(Error::EvaluationError(
                        EvaluationError::UnexpectedConfigurationParseError,
                    ));
                };
                ConditionCheck::Comparison {
                    operator,
                    comparand: condition_value,
                }
            }
            ConditionOperator::OneOf | ConditionOperator::NotOneOf => {
                let expected_membership = condition.operator == ConditionOperator::OneOf;
                let values = match condition.value {
                    ConditionValue::Multiple(v) => v,
                    _ => {
                        log::warn!(
                            "failed to parse condition: membership condition with non-array value: {:?}",
                            condition.value
                        );
                        return Err(Error::EvaluationError(
                            EvaluationError::UnexpectedConfigurationParseError,
                        ));
                    }
                };
                ConditionCheck::Membership {
                    expected_membership,
                    values,
                }
            }
            ConditionOperator::IsNull => {
                let Some(expected_null) = condition.value.singleton().and_then(|v| v.as_boolean())
                else {
                    log::warn!(
                        "failed to parse condition: IS_NULL condition with non-boolean condition value"
                    );
                    return Err(Error::EvaluationError(
                        EvaluationError::UnexpectedConfigurationParseError,
                    ));
                };
                ConditionCheck::Null { expected_null }
            }
        };
        Ok(Condition { attribute, check })
    }
}

/// Possible condition types.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum ConditionOperator {
    /// Matches regex. Condition value must be a regex string.
    Matches,
    /// Regex does not match. Condition value must be a regex string.
    NotMatches,
    /// Greater than or equal. Attribute and condition value must be numbers.
    Gte,
    /// Greater than. Attribute and condition value must be numbers.
    Gt,
    /// Less than or equal. Attribute and condition value must be numbers.
    Lte,
    /// Less than. Attribute and condition value must be numbers.
    Lt,
    /// One of values. Condition value must be a list of strings. Match is case-sensitive.
    OneOf,
    /// Not one of values. Condition value must be a list of strings. Match is case-sensitive.
    ///
    /// Null/absent attributes fail this condition automatically. (i.e., `null NOT_ONE_OF ["hello"]`
    /// is `false`)
    NotOneOf,
    /// Null check.
    ///
    /// Condition value must be a boolean. If it's `true`, this is a null check. If it's `false`,
    /// this is a not null check.
    IsNull,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(missing_docs)]
pub(crate) enum ConditionValue {
    Single(SingleConditionValue),
    // Only string arrays are currently supported.
    Multiple(Box<[Box<str>]>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, derive_more::From)]
#[serde(untagged)]
#[allow(missing_docs)]
pub(crate) enum SingleConditionValue {
    Boolean(bool),
    Number(f64),
    String(Str),
}

impl SingleConditionValue {
    pub fn as_number(&self) -> Option<f64> {
        if let Self::Number(n) = self {
            Some(*n)
        } else {
            None
        }
    }

    pub fn as_boolean(&self) -> Option<bool> {
        if let Self::Boolean(v) = self {
            Some(*v)
        } else {
            None
        }
    }
}

impl ConditionValue {
    pub fn singleton(&self) -> Option<SingleConditionValue> {
        match self {
            ConditionValue::Single(v) => Some(v.clone()),
            ConditionValue::Multiple(arr) if arr.len() == 1 => {
                Some(SingleConditionValue::String(arr[0].as_ref().into()))
            }
            _ => None,
        }
    }
}

impl<T: Into<SingleConditionValue>> From<T> for ConditionValue {
    fn from(value: T) -> Self {
        Self::Single(value.into())
    }
}
impl From<Vec<String>> for ConditionValue {
    fn from(value: Vec<String>) -> Self {
        Self::Multiple(value.into_iter().map(|it| it.into()).collect())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub(crate) struct SplitWire {
    pub shards: Vec<ShardWire>,
    pub variation_key: Str,
    #[serde(default)]
    pub extra_logging: Option<Arc<HashMap<String, String>>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub(crate) struct ShardWire {
    pub salt: String,
    pub total_shards: u32,
    pub ranges: Box<[ShardRange]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub struct ShardRange {
    pub start: u32,
    pub end: u32,
}
impl ShardRange {
    pub(crate) fn contains(&self, v: u32) -> bool {
        self.start <= v && v < self.end
    }
}

#[cfg(test)]
mod tests {
    use super::{TryParse, UniversalFlagConfigWire};

    #[test]
    fn parse_flags_v1() {
        let json_content = {
            let path = if std::path::Path::new("tests/data/flags-v1.json").exists() {
                "tests/data/flags-v1.json"
            } else {
                "domains/ffe/libs/flagging/rust/evaluation/tests/data/flags-v1.json"
            };
            std::fs::read_to_string(path).unwrap()
        };
        let _ufc: UniversalFlagConfigWire = serde_json::from_str(&json_content).unwrap();
    }

    #[test]
    fn parse_partially_if_unexpected() {
        let ufc: UniversalFlagConfigWire = serde_json::from_str(
            r#"
              {
                "data": {
                  "type": "universal-flag-configuration",
                  "id": "1",
                  "attributes": {
                    "createdAt": "2024-07-18T00:00:00Z",
                    "format": "SERVER",
                    "environment": {"name": "test"},
                    "flags": {
                      "success": {
                        "key": "success",
                        "enabled": true,
                        "variationType": "BOOLEAN",
                        "variations": {},
                        "allocations": []
                      },
                      "fail_parsing": {
                        "key": "fail_parsing",
                        "enabled": true,
                        "variationType": "NEW_TYPE",
                        "variations": {},
                        "allocations": []
                      }
                    }
                  }
                }
              }
            "#,
        )
        .unwrap();
        assert!(
            matches!(
                ufc.data.attributes.flags.get("success").unwrap(),
                TryParse::Parsed(_)
            ),
            "{:?} should match TryParse::Parsed(_)",
            ufc.data.attributes.flags.get("success").unwrap()
        );
        assert!(
            matches!(
                ufc.data.attributes.flags.get("fail_parsing").unwrap(),
                TryParse::ParseFailed(_)
            ),
            "{:?} should match TryParse::ParseFailed(_)",
            ufc.data.attributes.flags.get("fail_parsing").unwrap()
        );
    }
}
