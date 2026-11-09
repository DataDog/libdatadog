// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Where a point was observed (`PORTABLE-METRICS.md#observation-point`).
//! The expected values come from the contract text, not from the projectors.

use libdd_ai_usage::{Json, OBSERVATION_POINTS, Profile, project_result};

const ATTRIBUTE: &str = "trajectory.observation.point";

const ATTEMPT: &str = r#""operation_name": "chat", "provider_name": "exampleai", "duration_seconds": 1, "streaming": false, "input_tokens": 10"#;
const BREAKDOWN: &str = r#""operation_name": "chat", "provider_name": "exampleai", "input_tokens": 10, "output_tokens": 10, "input_basis": "includes_cache", "cache_read_input_tokens": 10, "cache_write_input_tokens": 0"#;
const STREAM: &str = r#""operation_name": "chat", "provider_name": "exampleai", "streaming": true, "output_chunk_intervals_seconds": [0.1, 0.15]"#;
const GATEWAY: &str = r#""operation_name": "chat", "duration_seconds": 0.4, "provider_operations": 0, "provider_operation_coverage": "complete", "retries": 0, "fallbacks": 0, "cache_outcomes": ["hit"]"#;

/// The profile, an observation without the member, and its point count.
const PROVIDER_PROFILES: [(Profile, &str, usize); 3] = [
    (Profile::ProviderAttempt, ATTEMPT, 3),
    (Profile::TokenBreakdown, BREAKDOWN, 3),
    (Profile::ProviderStreaming, STREAM, 2),
];

/// The value of the attribute on each point, or the rejection code.
fn observed(
    profile: Profile,
    members: &str,
    stated: &str,
) -> Result<Vec<Option<String>>, &'static str> {
    let text = format!("{{{members}{stated}}}");
    let input = Json::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
    let projection = project_result(profile, &input).map_err(|error| error.code().as_str())?;
    Ok(projection
        .points
        .iter()
        .map(|point| point.attributes.get(ATTRIBUTE).cloned())
        .collect())
}

#[test]
fn a_stated_point_is_on_every_point() {
    assert_eq!(OBSERVATION_POINTS.len(), 7);
    for (profile, members, count) in PROVIDER_PROFILES {
        for point in OBSERVATION_POINTS {
            let stated = format!(r#", "observation_point": "{point}""#);
            assert_eq!(
                observed(profile, members, &stated),
                Ok(vec![Some(point.to_string()); count]),
                "{profile:?} {point}"
            );
        }
    }
}

#[test]
fn an_absent_point_is_rejected() {
    for (profile, members, _) in PROVIDER_PROFILES {
        for stated in ["", r#", "observation_point": null"#] {
            assert_eq!(
                observed(profile, members, stated),
                Err("required_field_missing"),
                "{profile:?} {stated}"
            );
        }
    }
}
#[test]
fn a_value_outside_the_list_is_rejected() {
    for (profile, members, _) in PROVIDER_PROFILES {
        for (value, code) in [
            (r#""sidecar""#, "value_not_allowed"),
            (r#""""#, "value_not_allowed"),
            (r#""Gateway""#, "value_not_allowed"),
            (r#""gateway\n""#, "value_not_allowed"),
            ("7", "field_type_invalid"),
            (r#"["gateway"]"#, "field_type_invalid"),
            ("true", "field_type_invalid"),
        ] {
            let stated = format!(r#", "observation_point": {value}"#);
            assert_eq!(
                observed(profile, members, &stated),
                Err(code),
                "{profile:?} {value}"
            );
        }
    }
}

#[test]
fn gateway_request_points_always_say_gateway() {
    for stated in [
        "",
        r#", "observation_point": null"#,
        r#", "observation_point": "gateway""#,
    ] {
        assert_eq!(
            observed(Profile::GatewayRequest, GATEWAY, stated),
            Ok(vec![Some("gateway".to_string()); 5]),
            "{stated}"
        );
    }
    for (value, code) in [
        (r#""network_proxy""#, "value_not_allowed"),
        (r#""sidecar""#, "value_not_allowed"),
        (r#""""#, "value_not_allowed"),
        ("1", "field_type_invalid"),
    ] {
        let stated = format!(r#", "observation_point": {value}"#);
        assert_eq!(
            observed(Profile::GatewayRequest, GATEWAY, &stated),
            Err(code),
            "{value}"
        );
    }
}
