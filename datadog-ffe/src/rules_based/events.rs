use std::{collections::HashMap, sync::Arc};

use serde::Serialize;

use crate::rules_based::{
    SdkMetadata, Str,
    attributes::{Attributes, CategoricalAttribute, NumericAttribute},
};

/// Events that can be emitted during evaluation of assignment or bandit. They need to be logged to
/// analytics storage and fed back to Eppo for analysis.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Events {
    pub assignment: Option<AssignmentEvent>,
    pub bandit: Option<BanditEvent>,
}

/// Common fields for the same split.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentEventBase {
    /// The key of the feature flag being assigned.
    pub feature_flag: Str,
    /// The key of the allocation that the subject was assigned to.
    pub allocation: Str,
    /// The key of the experiment associated with the assignment.
    pub experiment: String,
    /// The specific variation assigned to the subject.
    pub variation: Str,
    /// Additional metadata such as SDK language and version.
    pub meta_data: EventMetaData,
    /// Additional user-defined logging fields for capturing extra information related to the
    /// assignment.
    #[serde(flatten)]
    pub extra_logging: Arc<HashMap<String, String>>,
}

/// Represents an event capturing the assignment of a feature flag to a subject and its logging
/// details.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentEvent {
    #[serde(flatten)]
    #[serde(serialize_with = "serialize_arc")]
    pub base: Arc<AssignmentEventBase>,
    /// The key identifying the subject receiving the assignment.
    pub subject: Str,
    /// Custom attributes of the subject relevant to the assignment.
    #[serde(serialize_with = "serialize_arc")]
    pub subject_attributes: Arc<Attributes>,
    /// The timestamp indicating when the assignment event occurred.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Bandit evaluation event that needs to be logged to analytics storage.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BanditEvent {
    pub flag_key: Str,
    pub bandit_key: Str,
    pub subject: Str,
    pub action: Str,
    pub action_probability: f64,
    pub optimality_gap: f64,
    pub model_version: Str,
    pub timestamp: String,
    #[serde(serialize_with = "serialize_arc")]
    pub subject_numeric_attributes: Arc<HashMap<Str, NumericAttribute>>,
    #[serde(serialize_with = "serialize_arc")]
    pub subject_categorical_attributes: Arc<HashMap<Str, CategoricalAttribute>>,
    #[serde(serialize_with = "serialize_arc")]
    pub action_numeric_attributes: Arc<HashMap<Str, NumericAttribute>>,
    #[serde(serialize_with = "serialize_arc")]
    pub action_categorical_attributes: Arc<HashMap<Str, CategoricalAttribute>>,
    pub meta_data: EventMetaData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventMetaData {
    pub sdk_name: &'static str,
    pub sdk_version: &'static str,
    pub core_version: &'static str,
}

impl From<SdkMetadata> for EventMetaData {
    fn from(sdk: SdkMetadata) -> EventMetaData {
        (&sdk).into()
    }
}

impl From<&SdkMetadata> for EventMetaData {
    fn from(sdk: &SdkMetadata) -> EventMetaData {
        EventMetaData {
            sdk_name: sdk.name,
            sdk_version: sdk.version,
            core_version: env!("CARGO_PKG_VERSION"),
        }
    }
}

fn serialize_arc<T, S>(arc: &Arc<T>, serializer: S) -> Result<S::Ok, S::Error>
where
    T: Serialize,
    S: serde::Serializer,
{
    arc.as_ref().serialize(serializer)
}
