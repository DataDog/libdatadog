// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Request documents: the JSON form in which a binding (the C interface, a
//! native module) hands this crate one observation, so that every binding
//! reads it alike.
//!
//! A request document is `{"profile": "{id}@{version}", "observation": {...},
//! "deployment_attributes": {...}, "metrics": [...]}`; the last two are
//! optional. A request is a JSON array of them: the provider attempts, token
//! breakdowns and gateway request of one client request, added together.

use serde::ser::{Serialize, SerializeStruct, Serializer};

use crate::batch::{Document, DocumentError, MetricBatch, Observation};
use crate::datadog::{DatadogSeries, datadog_series};
use crate::json::Json;
use crate::observation::{
    GatewayRequestObservation, ProviderAttemptObservation, ProviderStreamingObservation,
    TokenBreakdownObservation,
};
use crate::point::{ErrorCode, Issues, MetricError, Projection};
use crate::{Profile, project_result, retain_metrics, with_deployment_attributes_json};

fn invalid(message: impl Into<String>) -> MetricError {
    MetricError::new(ErrorCode::FieldTypeInvalid, message)
}

/// The profile a request document names in `profile`.
fn profile_of(document: &Json) -> Result<Profile, MetricError> {
    let id = document
        .get("profile")
        .and_then(Json::as_str)
        .ok_or_else(|| MetricError::new(ErrorCode::RequiredFieldMissing, "profile is required"))?;
    Profile::from_id_result(id)
}

/// A metric list as a request document's `metrics` states it: `None` for
/// JSON `null` (keep every metric), else the names of a JSON array of
/// strings. An empty array keeps no metric. Anything else is
/// `field_type_invalid`.
pub fn metric_list_from_json(value: &Json) -> Result<Option<Vec<&str>>, MetricError> {
    match value {
        Json::Null => Ok(None),
        Json::Array(names) => names
            .iter()
            .map(|name| {
                name.as_str()
                    .ok_or_else(|| invalid("metrics must be strings"))
            })
            .collect::<Result<Vec<&str>, MetricError>>()
            .map(Some),
        _ => Err(invalid("metrics must be an array of strings")),
    }
}

/// An observation decoded from a request document. A request holds a few,
/// so they are not boxed.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
enum OwnedObservation {
    Attempt(ProviderAttemptObservation),
    Streaming(ProviderStreamingObservation),
    Breakdown(TokenBreakdownObservation),
    Gateway(GatewayRequestObservation),
}

impl OwnedObservation {
    fn as_ref(&self) -> Observation<'_> {
        match self {
            OwnedObservation::Attempt(o) => Observation::ProviderAttempt(o),
            OwnedObservation::Streaming(o) => Observation::ProviderStreaming(o),
            OwnedObservation::Breakdown(o) => Observation::TokenBreakdown(o),
            OwnedObservation::Gateway(o) => Observation::GatewayRequest(o),
        }
    }
}

/// A request document decoded, borrowing its strings from the JSON: its
/// observation, its deployment attributes, and the metrics it keeps. Add it
/// to a batch through [`RequestDocument::document`] and
/// [`MetricBatch::try_add_request`], or a whole request with
/// [`MetricBatch::add_request_json`].
#[derive(Debug, Clone)]
pub struct RequestDocument<'j> {
    observation: OwnedObservation,
    /// A `None` value is one that is not a string; `null` values are left
    /// out.
    deployment_attributes: Vec<(&'j str, Option<&'j str>)>,
    /// `None`: the document states no list.
    metrics: Option<Vec<&'j str>>,
}

impl<'j> RequestDocument<'j> {
    /// Decode one request document. Its observation is checked as the
    /// profile's `from_json` checks it; the projection's own rejections
    /// (and an unknown deployment attribute) are reported when it is added.
    pub fn from_json(document: &'j Json) -> Result<Self, MetricError> {
        let profile = profile_of(document)?;
        let input = document.get(profile.input_key()).unwrap_or(&Json::Null);
        let observation = match profile {
            Profile::ProviderAttempt => {
                OwnedObservation::Attempt(ProviderAttemptObservation::from_json(input)?)
            }
            Profile::ProviderStreaming => {
                OwnedObservation::Streaming(ProviderStreamingObservation::from_json(input)?)
            }
            Profile::TokenBreakdown => {
                OwnedObservation::Breakdown(TokenBreakdownObservation::from_json(input)?)
            }
            Profile::GatewayRequest => {
                OwnedObservation::Gateway(GatewayRequestObservation::from_json(input)?)
            }
        };
        let deployment_attributes = match document.get("deployment_attributes") {
            None | Some(Json::Null) => Vec::new(),
            Some(Json::Object(members)) => members
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect(),
            Some(_) => return Err(invalid("deployment attributes must be an object")),
        };
        let metrics = metric_list_from_json(document.get("metrics").unwrap_or(&Json::Null))?;
        Ok(RequestDocument {
            observation,
            deployment_attributes,
            metrics,
        })
    }

    /// Decode a request, a JSON array of request documents. A rejected
    /// document's message names its index (`document {index}: ...`); when a
    /// document cannot be decoded, an earlier one that its projection
    /// rejects is the one reported, as adding the request would report it.
    pub fn from_request_json(request: &'j Json) -> Result<Vec<Self>, MetricError> {
        let documents = request
            .as_array()
            .ok_or_else(|| invalid("a request must be an array of documents"))?;
        let mut decoded = Vec::with_capacity(documents.len());
        for (at, document) in documents.iter().enumerate() {
            match RequestDocument::from_json(document) {
                Ok(document) => decoded.push(document),
                Err(error) => {
                    // Which metrics are kept never changes a rejection.
                    let earlier: Vec<Document<'_>> = decoded
                        .iter()
                        .map(|document| document.document(None))
                        .collect();
                    MetricBatch::new().try_add_request(&earlier)?;
                    return Err(MetricError::new(
                        error.code(),
                        format!("document {at}: {}", error.message()),
                    ));
                }
            }
        }
        Ok(decoded)
    }

    /// The profile of the document's observation.
    pub fn profile(&self) -> Profile {
        self.observation.as_ref().profile()
    }

    /// The document as [`MetricBatch::try_add_request`] adds it, keeping
    /// `default_metrics` (`None`: every metric) when it states no list of
    /// its own.
    pub fn document<'d>(&'d self, default_metrics: Option<&'d [&'d str]>) -> Document<'d> {
        Document {
            observation: self.observation.as_ref(),
            deployment_attributes: &self.deployment_attributes,
            metrics: self.metrics.as_deref().or(default_metrics),
        }
    }
}

impl From<DocumentError> for MetricError {
    /// The document's error, its message prefixed with the document's index
    /// (`document {index}: ...`).
    fn from(error: DocumentError) -> Self {
        MetricError::new(
            error.error.code(),
            format!("document {}: {}", error.document, error.error.message()),
        )
    }
}

impl MetricBatch {
    /// Add a request as a binding hands it over: `request` is a JSON array
    /// of request documents ([`RequestDocument::from_request_json`]), and a
    /// document that states no `metrics` keeps `default_metrics` (`None`:
    /// every metric). The documents are added together, as
    /// [`MetricBatch::try_add_request`] adds them: returns whether the
    /// request was added and each document's issues. A rejection's message
    /// names the document's index.
    pub fn add_request_json(
        &mut self,
        request: &Json,
        default_metrics: Option<&[&str]>,
    ) -> Result<(bool, Vec<Issues>), MetricError> {
        let decoded = RequestDocument::from_request_json(request)?;
        let documents: Vec<Document<'_>> = decoded
            .iter()
            .map(|document| document.document(default_metrics))
            .collect();
        Ok(self.try_add_request(&documents)?)
    }
}

/// Project one request document: its observation under its profile, its
/// deployment attributes applied after projection, and only the metrics its
/// `metrics` list names.
fn project_request(document: &Json) -> Result<(Profile, Projection), MetricError> {
    let profile = profile_of(document)?;
    let observation = document.get(profile.input_key()).unwrap_or(&Json::Null);
    let projection = project_result(profile, observation)?;
    let deployment = document.get("deployment_attributes").unwrap_or(&Json::Null);
    let mut projection = with_deployment_attributes_json(projection, deployment)?;
    if let Some(names) = metric_list_from_json(document.get("metrics").unwrap_or(&Json::Null))? {
        retain_metrics(&mut projection, &names);
    }
    Ok((profile, projection))
}

/// One request document projected to the series it is submitted to the
/// Datadog API as, with the issues raised beside them.
///
/// Serialized as `{"issues": [...], "series": [...]}`, each series as
/// [`DatadogSeries`] serializes.
#[derive(Debug, Clone, PartialEq)]
pub struct DatadogProjection {
    pub series: Vec<DatadogSeries>,
    pub issues: Issues,
}

impl DatadogProjection {
    /// Project the request document `document` (see
    /// [`RequestDocument::from_json`]) into its current (not historical)
    /// Datadog series.
    pub fn from_request_json(document: &Json) -> Result<Self, MetricError> {
        let (profile, projection) = project_request(document)?;
        let series = datadog_series(profile, &projection.points, &projection.resource, false)?;
        Ok(DatadogProjection {
            series,
            issues: projection.issues,
        })
    }
}

impl Serialize for DatadogProjection {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serializer.serialize_struct("DatadogProjection", 2)?;
        out.serialize_field("issues", &self.issues)?;
        out.serialize_field("series", &self.series)?;
        out.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ATTEMPT: &str = r#"{"profile": "gen_ai.client.provider_attempt@0.1.0",
        "observation": {"operation_name": "chat", "provider_name": "openai",
            "duration_seconds": 0.5, "streaming": false, "input_tokens": 10,
            "input_basis": "includes_cache", "output_tokens": 2,
            "output_basis": "includes_reasoning", "observation_point": "gateway"},
        "deployment_attributes": {"user.id": "ada@example.com"}}"#;

    fn json(text: &str) -> Json {
        Json::parse(text).unwrap()
    }

    #[test]
    fn metric_lists() {
        assert_eq!(metric_list_from_json(&Json::Null), Ok(None));
        assert_eq!(metric_list_from_json(&json("[]")), Ok(Some(vec![])));
        assert_eq!(
            metric_list_from_json(&json(r#"["a", "b"]"#)),
            Ok(Some(vec!["a", "b"]))
        );
        let error = metric_list_from_json(&json("[1]")).unwrap_err();
        assert_eq!(error.code(), ErrorCode::FieldTypeInvalid);
        assert_eq!(error.message(), "metrics must be strings");
        let error = metric_list_from_json(&json(r#""a""#)).unwrap_err();
        assert_eq!(error.message(), "metrics must be an array of strings");
    }

    #[test]
    fn known_metrics_per_profile() {
        let all = crate::known_metrics("").unwrap();
        let each: Vec<&str> = Profile::ALL
            .iter()
            .flat_map(|profile| profile.metric_names().iter().copied())
            .collect();
        assert_eq!(all, each);
        assert_eq!(
            crate::known_metrics(Profile::ProviderStreaming.id()).unwrap(),
            Profile::ProviderStreaming.metric_names()
        );
        let error = crate::known_metrics("nope").unwrap_err();
        assert_eq!(error.code(), ErrorCode::ProfileInvalid);
        assert_eq!(error.message(), "unsupported profile: nope");
    }

    #[test]
    fn a_document_decodes() {
        let document = json(ATTEMPT);
        let decoded = RequestDocument::from_json(&document).unwrap();
        assert_eq!(decoded.profile(), Profile::ProviderAttempt);
        let as_added = decoded.document(Some(&["gen_ai.client.inference.duration"]));
        assert_eq!(
            as_added.deployment_attributes,
            &[("user.id", Some("ada@example.com"))]
        );
        assert_eq!(
            as_added.metrics,
            Some(&["gen_ai.client.inference.duration"][..])
        );
        assert_eq!(decoded.document(None).metrics, None);
    }

    #[test]
    fn a_document_s_own_metrics_win_over_the_defaults() {
        let document = json(&ATTEMPT.replace(
            r#""deployment_attributes""#,
            r#""metrics": [], "deployment_attributes""#,
        ));
        let decoded = RequestDocument::from_json(&document).unwrap();
        assert_eq!(decoded.document(Some(&["x"])).metrics, Some(&[][..]));
    }

    #[test]
    fn rejected_documents() {
        let reject = |text: &str| RequestDocument::from_json(&json(text)).unwrap_err();
        let error = reject("{}");
        assert_eq!(error.code(), ErrorCode::RequiredFieldMissing);
        assert_eq!(error.message(), "profile is required");
        let error = reject(r#"{"profile": "nope@1"}"#);
        assert_eq!(error.code(), ErrorCode::ProfileInvalid);
        assert_eq!(error.message(), "unsupported profile: nope@1");
        let error = reject(&ATTEMPT.replace(r#"{"user.id": "ada@example.com"}"#, "[]"));
        assert_eq!(error.code(), ErrorCode::FieldTypeInvalid);
        assert_eq!(error.message(), "deployment attributes must be an object");
    }

    #[test]
    fn a_request_names_the_rejected_document() {
        let error = RequestDocument::from_request_json(&json("{}")).unwrap_err();
        assert_eq!(error.message(), "a request must be an array of documents");
        let request = json(&format!("[{ATTEMPT}, {{}}]"));
        let error = RequestDocument::from_request_json(&request).unwrap_err();
        assert_eq!(error.message(), "document 1: profile is required");
        // An earlier document its projection rejects is reported first.
        let bad = ATTEMPT.replace(r#""user.id""#, r#""not.an.attribute""#);
        let request = json(&format!("[{bad}, {{}}]"));
        let error = RequestDocument::from_request_json(&request).unwrap_err();
        assert_eq!(error.code(), ErrorCode::DeploymentAttributeUnknown);
        assert!(error.message().starts_with("document 0: "), "{error:?}");
    }

    #[test]
    fn a_request_is_added_with_default_metrics() {
        let request = json(&format!("[{ATTEMPT}]"));
        let mut batch = MetricBatch::new();
        let (added, issues) = batch
            .add_request_json(&request, Some(&["gen_ai.client.inference.duration"]))
            .unwrap();
        assert!(added);
        assert_eq!(issues.len(), 1);
        assert_eq!(batch.len(), 1);
        let (added, _) = batch.add_request_json(&request, None).unwrap();
        assert!(added);
        assert!(batch.len() > 2);
    }

    #[test]
    fn a_document_error_names_its_index() {
        let error = MetricError::from(DocumentError {
            document: 3,
            error: MetricError::new(ErrorCode::FieldTypeInvalid, "bad"),
        });
        assert_eq!(error.code(), ErrorCode::FieldTypeInvalid);
        assert_eq!(error.message(), "document 3: bad");
    }

    #[test]
    fn a_document_projects_to_datadog_series() {
        let projection = DatadogProjection::from_request_json(&json(ATTEMPT)).unwrap();
        assert!(projection.issues.is_empty());
        let rate = projection
            .series
            .iter()
            .find(|s| s.name == "gen_ai.client.inference.usage.input_tokens")
            .unwrap();
        assert_eq!(rate.interval, Some(10));
        let value = serde_json::to_value(&projection).unwrap();
        assert_eq!(value["issues"], serde_json::json!([]));
        assert!(value["series"].as_array().unwrap().iter().any(|s| {
            s["name"] == "gen_ai.client.inference.usage.input_tokens"
                && s["type"] == "rate"
                && s["interval"] == 10
                && s["value"] == 1.0
        }));
        // Keys are written sorted, as a `serde_json::Value` writes them.
        let text = serde_json::to_string(&projection).unwrap();
        assert_eq!(text, value.to_string());
    }

    #[test]
    fn a_projected_document_keeps_its_metrics() {
        let document = json(&ATTEMPT.replace(
            r#""deployment_attributes""#,
            r#""metrics": ["gen_ai.client.inference.duration"], "deployment_attributes""#,
        ));
        let projection = DatadogProjection::from_request_json(&document).unwrap();
        // The Histogram's sample, with its `.count` and `.sum` Rates.
        let mut names: Vec<(&str, Option<u32>)> = projection
            .series
            .iter()
            .map(|s| (s.name.as_str(), s.interval))
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                ("gen_ai.client.inference.duration", None),
                ("gen_ai.client.inference.duration.count", Some(10)),
                ("gen_ai.client.inference.duration.sum", Some(10)),
            ]
        );
        let error = DatadogProjection::from_request_json(&json(r#"{"profile": 1}"#)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::RequiredFieldMissing);
    }
}
