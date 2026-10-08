// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Every valid conformance observation, with each field removed, nulled, or
//! replaced by a value of another type or out of range. The projector must
//! never panic, and whatever it accepts must stay within the registry: only
//! the profile's metrics, only allowed attributes, and finite, non-negative
//! values.

use std::fs;
use std::path::{Path, PathBuf};

use libdd_ai_usage::{
    Json, JsonObject, Profile, Registry, project_result, with_deployment_attributes_json,
};

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn read_json(path: &Path) -> Json {
    Json::parse(&fs::read_to_string(path).unwrap()).unwrap()
}

const DIRS: [(&str, Profile); 4] = [
    ("provider-attempt", Profile::ProviderAttempt),
    ("provider-streaming", Profile::ProviderStreaming),
    ("provider-token-breakdown", Profile::TokenBreakdown),
    ("gateway-request", Profile::GatewayRequest),
];

/// The replacement values tried for every field.
fn replacements() -> Vec<Json> {
    let mut object = JsonObject::new();
    object.insert("text".into(), Json::Number(1.0));
    vec![
        Json::Null,
        Json::Bool(true),
        Json::Bool(false),
        Json::Number(0.0),
        Json::Number(-1.0),
        Json::Number(0.5),
        Json::Number(9_007_199_254_740_993.0),
        Json::Number(f64::INFINITY),
        Json::Number(f64::NEG_INFINITY),
        Json::String(String::new()),
        Json::String("Not An Identifier!".into()),
        Json::String("gateway".into()),
        Json::String("estimated".into()),
        Json::Array(vec![Json::String("hit".into())]),
        Json::Array(vec![Json::Number(1.0)]),
        Json::Object(object),
    ]
}

fn variants(observation: &JsonObject) -> Vec<Json> {
    let mut out = Vec::new();
    for key in observation.keys() {
        let mut removed = observation.clone();
        removed.remove(key);
        out.push(Json::Object(removed));
        for value in replacements() {
            let mut replaced = observation.clone();
            replaced.insert(key.clone(), value);
            out.push(Json::Object(replaced));
        }
    }
    out
}

#[test]
#[cfg_attr(
    miri,
    ignore = "tens of thousands of projections; the rule tests cover these paths under Miri"
)]
fn perturbed_observations_never_panic_and_stay_within_the_registry() {
    let registry =
        Registry::from_json(&fs::read_to_string(data().join("registry.json")).unwrap()).unwrap();
    let mut projected = 0;
    let mut rejected = 0;
    let mut failures = Vec::new();
    for (dir, profile) in DIRS {
        let allowed = registry.profile_metrics(profile.id()).unwrap_or_default();
        let mut paths: Vec<PathBuf> = fs::read_dir(data().join(dir).join("valid"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        paths.sort();
        for path in paths {
            let fixture = read_json(&path);
            let Some(Json::Object(observation)) = fixture.get("observation") else {
                continue;
            };
            let attributes = fixture.get("deployment_attributes").unwrap_or(&Json::Null);
            for variant in variants(observation) {
                let result = project_result(profile, &variant)
                    .and_then(|projection| with_deployment_attributes_json(projection, attributes));
                let projection = match result {
                    Ok(projection) => projection,
                    Err(_) => {
                        rejected += 1;
                        continue;
                    }
                };
                projected += 1;
                let findings = registry.check_points(&projection.points);
                if !findings.is_empty() {
                    failures.push(format!("{}: {variant:?}: {findings:?}", path.display()));
                }
                for point in &projection.points {
                    if !allowed.contains(&point.name) {
                        failures.push(format!("{}: {} not in profile", path.display(), point.name));
                    }
                    if !point.value.is_finite() || point.value < 0.0 {
                        failures.push(format!(
                            "{}: {} has value {}",
                            path.display(),
                            point.name,
                            point.value
                        ));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} problems:\n{}",
        failures.len(),
        failures.join("\n")
    );
    // Both paths are exercised.
    assert!(projected > 1_000, "{projected} projected");
    assert!(rejected > 1_000, "{rejected} rejected");
}

#[test]
fn deeply_nested_and_garbage_json_is_refused_without_panic() {
    let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
    assert!(Json::parse(&deep).is_err());
    for text in [
        "",
        "{",
        "{\"a\":}",
        "nul",
        "1e",
        "\"\\u12\"",
        "[1,]",
        "{\"a\":1}x",
    ] {
        assert!(Json::parse(text).is_err(), "{text}");
    }
    for profile in Profile::ALL {
        for input in [Json::Null, Json::Bool(true), Json::Array(vec![])] {
            assert!(project_result(profile, &input).is_err());
        }
    }
}
