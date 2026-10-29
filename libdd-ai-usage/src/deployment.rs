// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Deployment attributes (`PORTABLE-METRICS.md#deployment-attributes`):
//! opt-in dimensions a deployment adds after projection.

use std::collections::BTreeMap;

use crate::json::Json;
use crate::point::{ErrorCode, IssueCode, MetricError, Projection, merge_counter_points, reject};

/// Where OTLP carries a deployment attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    /// On every data point.
    Point,
    /// On the resource, and on no point.
    Resource,
}

impl Carrier {
    pub fn as_str(self) -> &'static str {
        match self {
            Carrier::Point => "point",
            Carrier::Resource => "resource",
        }
    }
}

/// The closed list of deployment attributes. The registry states the same
/// list under `deployment_attributes`; a test keeps the two equal.
pub const DEPLOYMENT_ATTRIBUTES: [(&str, Carrier); 11] = [
    ("host.name", Carrier::Resource),
    ("service.name", Carrier::Resource),
    ("trajectory.client_source", Carrier::Point),
    // The three a gateway sets on its own points
    // (`PORTABLE-METRICS.md#gateway-attributes`).
    ("trajectory.gateway.destination.id", Carrier::Point),
    ("trajectory.gateway.key.alias", Carrier::Point),
    ("trajectory.gateway.route", Carrier::Point),
    ("trajectory.request.api_key_id", Carrier::Point),
    ("trajectory.request.project_id", Carrier::Point),
    ("trajectory.request.provider_account_id", Carrier::Point),
    ("trajectory.team.id", Carrier::Point),
    ("user.id", Carrier::Point),
];

/// The carrier of a deployment attribute, or `None` for a name outside the
/// list.
pub fn deployment_attribute_carrier(name: &str) -> Option<Carrier> {
    DEPLOYMENT_ATTRIBUTES
        .iter()
        .find(|(listed, _)| *listed == name)
        .map(|(_, carrier)| *carrier)
}

/// `^[A-Za-z0-9][A-Za-z0-9._@+:/'|-]{0,127}$`: an identifier, never free
/// text.
pub fn is_deployment_value(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.iter().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'.' | b'_' | b'@' | b'+' | b':' | b'/' | b'\'' | b'|' | b'-'
                )
        })
}

/// Add deployment attributes to a projection.
///
/// A name outside the list rejects the input (`deployment_attribute_unknown`).
/// A value outside the grammar drops that attribute and raises
/// `deployment_attribute_dropped`; the points are kept. A point-carried
/// attribute goes on every point that does not already carry it, and a
/// resource-carried one is returned in `resource` and goes on no point.
/// Counter points that the attributes make equal are merged into one, with
/// the exact sum. Names are taken in sorted order.
pub fn with_deployment_attributes(
    projection: Projection,
    attributes: &BTreeMap<String, String>,
) -> Result<Projection, MetricError> {
    apply(
        projection,
        attributes
            .iter()
            .map(|(name, value)| (name.as_str(), Some(value.as_str()))),
    )
}

/// [`with_deployment_attributes`] for attributes stated as a JSON object,
/// as a configuration file or a conformance case states them. A `null`
/// value is absent. A value that is not a string, such as a number or a
/// boolean, breaks the grammar like any other: it is dropped with
/// `deployment_attribute_dropped`, and is not a rejection. `null` in place
/// of the object is no attributes; another JSON type is `field_type_invalid`.
pub fn with_deployment_attributes_json(
    projection: Projection,
    attributes: &Json,
) -> Result<Projection, MetricError> {
    match attributes {
        Json::Null => apply(projection, std::iter::empty()),
        Json::Object(members) => apply(
            projection,
            members
                .iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(name, value)| (name.as_str(), value.as_str())),
        ),
        _ => reject(
            ErrorCode::FieldTypeInvalid,
            "deployment attributes must be an object",
        ),
    }
}

/// `attributes` yields each name with its value, or `None` for a value that
/// is not a string.
fn apply<'a>(
    mut projection: Projection,
    attributes: impl Iterator<Item = (&'a str, Option<&'a str>)>,
) -> Result<Projection, MetricError> {
    let mut carried = Vec::new();
    for (name, value) in attributes {
        match deployment_attribute_carrier(name) {
            Some(carrier) => carried.push((name, value, carrier)),
            None => {
                return reject(
                    ErrorCode::DeploymentAttributeUnknown,
                    format!("{name} is not a deployment attribute"),
                );
            }
        }
    }
    for (name, value, carrier) in carried {
        let Some(value) = value.filter(|value| is_deployment_value(value)) else {
            projection
                .issues
                .insert(IssueCode::DeploymentAttributeDropped);
            continue;
        };
        match carrier {
            Carrier::Resource => {
                projection
                    .resource
                    .insert(name.to_string(), value.to_string());
            }
            Carrier::Point => {
                for point in &mut projection.points {
                    point
                        .attributes
                        .entry(name.to_string())
                        .or_insert_with(|| value.to_string());
                }
            }
        }
    }
    // Counter points the attributes made equal are one point.
    projection.points = merge_counter_points(std::mem::take(&mut projection.points));
    Ok(projection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point::{Attributes, MetricPoint};

    #[test]
    fn the_value_grammar_admits_identifiers_and_no_free_text() {
        for good in [
            "u-7f3a9c",
            "auth0|ada@example.com",
            "unknown_service:node",
            "https://example.com/acct",
            "o'brien+tag",
            "9",
        ] {
            assert!(is_deployment_value(good), "{good}");
        }
        assert!(is_deployment_value(&"t".repeat(128)));
        for bad in [
            "",
            "Ada Lovelace",
            "caf\u{e9}",
            "-lead",
            ".lead",
            "a\tb",
            "a\u{0}",
        ] {
            assert!(!is_deployment_value(bad), "{bad:?}");
        }
        assert!(!is_deployment_value(&"t".repeat(129)));
    }

    #[test]
    fn an_unknown_name_rejects_even_beside_a_bad_value() {
        let attributes: BTreeMap<String, String> = [
            ("session.id".to_string(), "sess-1".to_string()),
            ("user.id".to_string(), "Ada Lovelace".to_string()),
        ]
        .into();
        let error = with_deployment_attributes(Projection::default(), &attributes).unwrap_err();
        assert_eq!(error.code(), ErrorCode::DeploymentAttributeUnknown);
    }

    #[test]
    fn a_point_keeps_its_own_value_and_resource_attributes_stay_off_points() {
        let mut own = Attributes::new();
        own.insert("trajectory.client_source".into(), "own".into());
        let projection = Projection {
            points: vec![
                MetricPoint::counter("a", "{x}", 1.0, own),
                MetricPoint::counter("b", "{x}", 1.0, Attributes::new()),
            ],
            ..Projection::default()
        };
        let attributes: BTreeMap<String, String> = [
            ("trajectory.client_source".to_string(), "deploy".to_string()),
            ("host.name".to_string(), "build-17".to_string()),
            ("service.name".to_string(), "bad value".to_string()),
        ]
        .into();
        let result = with_deployment_attributes(projection, &attributes).unwrap();
        assert_eq!(
            result.points[0].attributes["trajectory.client_source"],
            "own"
        );
        assert_eq!(
            result.points[1].attributes["trajectory.client_source"],
            "deploy"
        );
        assert!(!result.points[1].attributes.contains_key("host.name"));
        assert_eq!(result.resource.len(), 1);
        assert_eq!(result.resource["host.name"], "build-17");
        assert!(
            result
                .issues
                .contains(&IssueCode::DeploymentAttributeDropped)
        );
    }

    #[test]
    fn a_value_that_is_not_a_string_is_dropped_and_null_is_absent() {
        let projection = Projection {
            points: vec![MetricPoint::counter("a", "{x}", 1.0, Attributes::new())],
            ..Projection::default()
        };
        let apply = |text: &str| {
            let attributes = Json::parse(text).unwrap();
            with_deployment_attributes_json(projection.clone(), &attributes)
                .map(|p| {
                    (
                        p.points[0].attributes.len(),
                        p.resource.len(),
                        p.issues.len(),
                    )
                })
                .map_err(|error| error.code().as_str())
        };
        assert_eq!(
            apply(r#"{"user.id": "u-1", "host.name": "h"}"#),
            Ok((1, 1, 0))
        );
        for dropped in ["5", "true", "[\"u\"]", "{}", "\"two words\""] {
            let text = format!(r#"{{"user.id": {dropped}, "trajectory.team.id": "t"}}"#);
            assert_eq!(apply(&text), Ok((1, 0, 1)), "{dropped}");
        }
        assert_eq!(apply(r#"{"user.id": null}"#), Ok((0, 0, 0)));
        assert_eq!(apply("null"), Ok((0, 0, 0)));
        assert_eq!(apply("{}"), Ok((0, 0, 0)));
        // A null member is absent also under a name outside the list.
        assert_eq!(apply(r#"{"session.id": null}"#), Ok((0, 0, 0)));
        assert_eq!(
            apply(r#"{"session.id": 5}"#),
            Err("deployment_attribute_unknown")
        );
        assert_eq!(apply("[]"), Err("field_type_invalid"));
    }

    #[test]
    fn points_the_attributes_make_equal_are_one_point() {
        let mut own = Attributes::new();
        own.insert("trajectory.client_source".into(), "c".into());
        let projection = Projection {
            points: vec![
                MetricPoint::counter("a", "{x}", 1.0, own.clone()),
                MetricPoint::histogram("h", "s", 1.0, own),
                MetricPoint::counter("a", "{x}", 2.0, Attributes::new()),
                MetricPoint::histogram("h", "s", 1.0, Attributes::new()),
            ],
            ..Projection::default()
        };
        let attributes: BTreeMap<String, String> =
            [("trajectory.client_source".to_string(), "c".to_string())].into();
        let result = with_deployment_attributes(projection, &attributes).unwrap();
        let values: Vec<(&str, f64)> = result
            .points
            .iter()
            .map(|p| (p.name.as_str(), p.value))
            .collect();
        assert_eq!(values, [("a", 3.0), ("h", 1.0), ("h", 1.0)]);
    }
}
