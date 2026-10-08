// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The portable metric registry, the profile identity of a metric
//! (`PORTABLE-METRICS.md#schema-version-on-points`), and checks of projected
//! points against the registry.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Deserialize;
use serde_json::Value;

use crate::deployment::Carrier;
use crate::point::{ErrorCode, MetricError, MetricPoint, reject};

/// The attribute that carries the profile identifier: on the OTLP scope, or
/// on each point when the SDK cannot set scope attributes.
pub const PROFILE_ATTRIBUTE: &str = "trajectory.profile";

#[derive(Debug, Clone, Deserialize)]
pub struct Registry {
    pub registry_version: String,
    pub profiles: Vec<ProfileDefinition>,
    #[serde(default)]
    pub deployment_attributes: Vec<DeploymentAttributeDefinition>,
    pub metrics: Vec<MetricDefinition>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProfileDefinition {
    pub id: String,
    pub version: String,
    pub metric_names: Vec<String>,
}

impl ProfileDefinition {
    /// `{id}@{version}`.
    pub fn identifier(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeploymentAttributeDefinition {
    pub name: String,
    pub carrier: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetricOrigin {
    pub kind: String,
    #[serde(default)]
    pub specification: String,
    #[serde(default)]
    pub revision: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttributeDefinition {
    pub name: String,
    pub requirement: String,
    pub cardinality: String,
    #[serde(default)]
    pub allowed_values: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetricDefinition {
    pub name: String,
    pub origin: MetricOrigin,
    pub instrument: String,
    pub unit: String,
    pub attributes: Vec<AttributeDefinition>,
    /// Deprecated opaque backend metadata; backend mappings are separate documents.
    #[serde(default)]
    pub transport_bindings: Option<Value>,
}

/// A parsed profile identifier, `{id}@{version}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileIdentifier {
    pub id: String,
    pub version: String,
}

/// `0|[1-9][0-9]*`
fn is_version_number(part: &str) -> bool {
    !part.is_empty()
        && part.bytes().all(|b| b.is_ascii_digit())
        && (part == "0" || !part.starts_with('0'))
}

/// Parse `^([a-z][a-z0-9_.]*)@((0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*))$`.
/// Anything else is `profile_invalid`.
pub fn parse_profile(profile: &str) -> Result<ProfileIdentifier, MetricError> {
    let parsed = profile.split_once('@').filter(|(id, version)| {
        let mut id_bytes = id.bytes();
        let id_ok = id_bytes.next().is_some_and(|b| b.is_ascii_lowercase())
            && id_bytes
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.'));
        let parts: Vec<&str> = version.split('.').collect();
        id_ok && parts.len() == 3 && parts.iter().all(|part| is_version_number(part))
    });
    match parsed {
        Some((id, version)) => Ok(ProfileIdentifier {
            id: id.to_string(),
            version: version.to_string(),
        }),
        None => reject(
            ErrorCode::ProfileInvalid,
            format!("{profile} is not a profile identifier of the form id@version"),
        ),
    }
}

/// What a registry or binding validator found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FindingCode {
    VersionInvalid,
    DuplicateMetric,
    NamespaceMismatch,
    UnboundedAttribute,
    UnknownProfileMetric,
    MetricProfileCount,
    DeploymentAttributeInvalid,
    UnknownMetric,
    InstrumentMismatch,
    UnitMismatch,
    AttributeNotAllowed,
    AttributeValueNotAllowed,
    RequiredAttributeMissing,
    ProfileAttributeMismatch,
    DocumentInvalid,
    UnknownField,
    BindingInvalid,
    DuplicateBinding,
    DuplicateDestinationName,
    DestinationNameMismatch,
    TypeMismatch,
    ScaleInvalid,
    CompanionInvalid,
    MissingBinding,
    ProfileTagMissing,
}

impl FindingCode {
    /// The stable string of the code.
    pub fn as_str(self) -> &'static str {
        match self {
            FindingCode::VersionInvalid => "version_invalid",
            FindingCode::DuplicateMetric => "duplicate_metric",
            FindingCode::NamespaceMismatch => "namespace_mismatch",
            FindingCode::UnboundedAttribute => "unbounded_attribute",
            FindingCode::UnknownProfileMetric => "unknown_profile_metric",
            FindingCode::MetricProfileCount => "metric_profile_count",
            FindingCode::DeploymentAttributeInvalid => "deployment_attribute_invalid",
            FindingCode::UnknownMetric => "unknown_metric",
            FindingCode::InstrumentMismatch => "instrument_mismatch",
            FindingCode::UnitMismatch => "unit_mismatch",
            FindingCode::AttributeNotAllowed => "attribute_not_allowed",
            FindingCode::AttributeValueNotAllowed => "attribute_value_not_allowed",
            FindingCode::RequiredAttributeMissing => "required_attribute_missing",
            FindingCode::ProfileAttributeMismatch => "profile_attribute_mismatch",
            FindingCode::DocumentInvalid => "document_invalid",
            FindingCode::UnknownField => "unknown_field",
            FindingCode::BindingInvalid => "binding_invalid",
            FindingCode::DuplicateBinding => "duplicate_binding",
            FindingCode::DuplicateDestinationName => "duplicate_destination_name",
            FindingCode::DestinationNameMismatch => "destination_name_mismatch",
            FindingCode::TypeMismatch => "type_mismatch",
            FindingCode::ScaleInvalid => "scale_invalid",
            FindingCode::CompanionInvalid => "companion_invalid",
            FindingCode::MissingBinding => "missing_binding",
            FindingCode::ProfileTagMissing => "profile_tag_missing",
        }
    }
}

/// One finding of a validator: a stable code, the name it is about, and a
/// diagnostic for people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub code: FindingCode,
    /// The metric, attribute, profile, or field the finding is about.
    pub subject: String,
    pub message: String,
}

impl Finding {
    pub(crate) fn new(code: FindingCode, subject: &str, message: impl Into<String>) -> Self {
        Self {
            code,
            subject: subject.to_string(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl Registry {
    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    pub fn metric(&self, name: &str) -> Option<&MetricDefinition> {
        self.metrics.iter().find(|m| m.name == name)
    }

    /// Metric names of `profile_id` (`id@version` or bare `id`).
    pub fn profile_metrics(&self, profile_id: &str) -> Option<&[String]> {
        self.profiles
            .iter()
            .find(|p| p.identifier() == profile_id || p.id == profile_id)
            .map(|p| p.metric_names.as_slice())
    }

    /// The `{id}@{version}` of the one profile that lists `metric`. A metric
    /// listed by no profile, or by more than one, is
    /// `metric_profile_mismatch`.
    pub fn profile_for_metric(&self, metric: &str) -> Result<String, MetricError> {
        let mut listing = self
            .profiles
            .iter()
            .filter(|p| p.metric_names.iter().any(|name| name == metric));
        match (listing.next(), listing.next()) {
            (Some(profile), None) => Ok(profile.identifier()),
            _ => reject(
                ErrorCode::MetricProfileMismatch,
                format!("{metric} is not listed by exactly one profile"),
            ),
        }
    }

    /// Whether `metric` may be exported under `profile`. The identifier is
    /// checked first (`profile_invalid`); a profile, by id and version, that
    /// does not list the metric is `metric_profile_mismatch`.
    pub fn check_profile_member(&self, profile: &str, metric: &str) -> Result<(), MetricError> {
        let parsed = parse_profile(profile)?;
        let listed = self.profiles.iter().any(|p| {
            p.id == parsed.id
                && p.version == parsed.version
                && p.metric_names.iter().any(|name| name == metric)
        });
        if listed {
            Ok(())
        } else {
            reject(
                ErrorCode::MetricProfileMismatch,
                format!("{profile} does not list {metric}"),
            )
        }
    }

    fn is_point_deployment_attribute(&self, name: &str) -> bool {
        self.deployment_attributes
            .iter()
            .any(|a| a.name == name && a.carrier == Carrier::Point.as_str())
    }

    /// Internal consistency of the registry.
    pub fn validate(&self) -> Vec<Finding> {
        let mut findings = Vec::new();
        if !is_semver(&self.registry_version) {
            findings.push(Finding::new(
                FindingCode::VersionInvalid,
                "registry_version",
                "registry_version must be a semantic version",
            ));
        }
        let mut names = BTreeSet::new();
        for metric in &self.metrics {
            if !names.insert(metric.name.as_str()) {
                findings.push(Finding::new(
                    FindingCode::DuplicateMetric,
                    &metric.name,
                    format!("duplicate metric name: {}", metric.name),
                ));
            }
            let namespace = match metric.origin.kind.as_str() {
                "opentelemetry" => Some("gen_ai."),
                "open_trajectory" => Some("trajectory."),
                _ => None,
            };
            if namespace.is_some_and(|prefix| !metric.name.starts_with(prefix)) {
                findings.push(Finding::new(
                    FindingCode::NamespaceMismatch,
                    &metric.name,
                    format!(
                        "{} metric is outside its namespace: {}",
                        metric.origin.kind, metric.name
                    ),
                ));
            }
            for attribute in &metric.attributes {
                if !matches!(attribute.cardinality.as_str(), "low" | "bounded") {
                    findings.push(Finding::new(
                        FindingCode::UnboundedAttribute,
                        &attribute.name,
                        format!(
                            "metric {} has an unbounded attribute: {}",
                            metric.name, attribute.name
                        ),
                    ));
                }
            }
        }
        let mut listed: BTreeMap<&str, usize> = BTreeMap::new();
        for profile in &self.profiles {
            for metric_name in &profile.metric_names {
                *listed.entry(metric_name.as_str()).or_default() += 1;
                if !names.contains(metric_name.as_str()) {
                    findings.push(Finding::new(
                        FindingCode::UnknownProfileMetric,
                        metric_name,
                        format!(
                            "profile {} references unknown metric {}",
                            profile.identifier(),
                            metric_name
                        ),
                    ));
                }
            }
        }
        for name in &names {
            let count = listed.get(name).copied().unwrap_or(0);
            if count != 1 {
                findings.push(Finding::new(
                    FindingCode::MetricProfileCount,
                    name,
                    format!("metric {name} is listed by {count} profiles, not exactly one"),
                ));
            }
        }
        let mut deployment = BTreeSet::new();
        for attribute in &self.deployment_attributes {
            let carrier_ok = matches!(attribute.carrier.as_str(), "point" | "resource");
            if !carrier_ok || !deployment.insert(attribute.name.as_str()) {
                findings.push(Finding::new(
                    FindingCode::DeploymentAttributeInvalid,
                    &attribute.name,
                    format!(
                        "deployment attribute {} is repeated or has an unknown carrier",
                        attribute.name
                    ),
                ));
            }
        }
        findings
    }

    /// Check projected points against their registry definitions: known
    /// metric, matching instrument and unit, no attribute outside the
    /// metric's list, allowed values respected, and required attributes
    /// present. Two kinds of attribute outside a metric's own list are
    /// allowed: a point-carried deployment attribute, and `trajectory.profile`
    /// when it names the profile that lists the metric. An empty result
    /// means the points stay within the registry.
    pub fn check_points(&self, points: &[MetricPoint]) -> Vec<Finding> {
        let mut findings = Vec::new();
        for point in points {
            let Some(metric) = self.metric(&point.name) else {
                findings.push(Finding::new(
                    FindingCode::UnknownMetric,
                    &point.name,
                    format!("unknown metric: {}", point.name),
                ));
                continue;
            };
            if metric.instrument != point.instrument.as_str() {
                findings.push(Finding::new(
                    FindingCode::InstrumentMismatch,
                    &point.name,
                    format!(
                        "{} must be a {}, not a {}",
                        point.name,
                        metric.instrument,
                        point.instrument.as_str()
                    ),
                ));
            }
            if metric.unit != point.unit {
                findings.push(Finding::new(
                    FindingCode::UnitMismatch,
                    &point.name,
                    format!("{} must use unit {}", point.name, metric.unit),
                ));
            }
            for (key, value) in &point.attributes {
                let definition = metric.attributes.iter().find(|a| a.name == *key);
                match definition {
                    Some(definition) => {
                        let allowed = definition
                            .allowed_values
                            .as_ref()
                            .is_none_or(|values| values.contains(value));
                        if !allowed {
                            findings.push(Finding::new(
                                FindingCode::AttributeValueNotAllowed,
                                key,
                                format!(
                                    "{} attribute {} has disallowed value {}",
                                    point.name, key, value
                                ),
                            ));
                        }
                    }
                    None if key == PROFILE_ATTRIBUTE => {
                        if self.check_profile_member(value, &point.name).is_err() {
                            findings.push(Finding::new(
                                FindingCode::ProfileAttributeMismatch,
                                key,
                                format!("{} is not a metric of profile {}", point.name, value),
                            ));
                        }
                    }
                    None if self.is_point_deployment_attribute(key) => {}
                    None => findings.push(Finding::new(
                        FindingCode::AttributeNotAllowed,
                        key,
                        format!("{} does not allow attribute {}", point.name, key),
                    )),
                }
            }
            for definition in &metric.attributes {
                if definition.requirement == "required"
                    && !point.attributes.contains_key(&definition.name)
                {
                    findings.push(Finding::new(
                        FindingCode::RequiredAttributeMissing,
                        &definition.name,
                        format!("{} requires attribute {}", point.name, definition.name),
                    ));
                }
            }
        }
        findings
    }
}

/// `^[0-9]+\.[0-9]+\.[0-9]+$`, the version shape of the registry schemas.
pub(crate) fn is_semver(value: &str) -> bool {
    let parts: Vec<&str> = value.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}
