use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::rules_based::Str;
use crate::rules_based::timestamp::Timestamp;
use crate::rules_based::ufc::{
    Assignment, AssignmentReason, ConfigurationFormat, Environment, VariationType,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrecomputedConfiguration {
    pub(crate) obfuscated: serde_bool::False,
    pub(crate) created_at: Timestamp,
    /// `format` is always `AssignmentFormat::Precomputed`.
    pub(crate) format: ConfigurationFormat,
    // Environment might be missing if configuration was absent during evaluation.
    pub(crate) environment: Option<Environment>,
    pub(crate) flags: HashMap</* flag_key: */ Str, PrecomputedAssignment>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PrecomputeAssignmentsResponse {
    pub data: PrecomputeAssignmentsResponseData,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PrecomputeAssignmentsResponseData {
    pub id: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub attributes: PrecomputedConfiguration,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PrecomputedAssignment {
    pub(crate) variation_type: PrecomputedVariationType,
    pub(crate) variation_value: serde_json::Value,
    pub(crate) do_log: bool,
    pub(crate) allocation_key: Str,
    pub(crate) variation_key: Str,
    pub(crate) extra_logging: Arc<HashMap<String, String>>,
    pub(crate) reason: AssignmentReason,
}

impl From<Assignment> for PrecomputedAssignment {
    fn from(assignment: Assignment) -> PrecomputedAssignment {
        PrecomputedAssignment {
            variation_type: assignment.value.variation_type().into(),
            variation_value: assignment.value.variation_value(),
            do_log: assignment.event.is_some(),
            allocation_key: assignment.allocation_key,
            variation_key: assignment.variation_key,
            extra_logging: assignment.extra_logging,
            reason: assignment.reason,
        }
    }
}

// Temporarily remap variation value to make released browser SDK work. See FFL-1239.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PrecomputedVariationType {
    String,
    Number,
    Boolean,
    Object,
}

impl From<VariationType> for PrecomputedVariationType {
    fn from(value: VariationType) -> Self {
        match value {
            VariationType::String => PrecomputedVariationType::String,
            VariationType::Integer => PrecomputedVariationType::Number,
            VariationType::Numeric => PrecomputedVariationType::Number,
            VariationType::Boolean => PrecomputedVariationType::Boolean,
            VariationType::Json => PrecomputedVariationType::Object,
        }
    }
}
