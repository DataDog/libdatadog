// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The shared metric cases in `tests/data`: every implemented profile's valid
//! and invalid cases, and the registry. Rejections are compared by error code,
//! never by message text. See `tests/data/README.md` for how points compare.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::registry::{FindingCode, Registry};
use libdd_ai_usage::{
    DEPLOYMENT_ATTRIBUTES, ErrorCode, IssueCode, Json, MetricPoint, Profile,
    project_fixture_result, sort_metric_points,
};

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn read_json(path: &Path) -> Json {
    Json::parse(&read_text(path)).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn registry() -> Registry {
    Registry::from_json(&read_text(&data().join("registry.json"))).unwrap()
}

fn fixtures(dir: &str, kind: &str) -> Vec<PathBuf> {
    let path = data().join(dir).join(kind);
    let mut files: Vec<PathBuf> = fs::read_dir(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    files
}

fn text<'a>(value: &'a Json, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Json::as_str)
}

fn strings(value: Option<&Json>) -> BTreeSet<String> {
    value
        .and_then(Json::as_array)
        .unwrap_or_default()
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect()
}

/// The valid and invalid case counts per profile. Every case in a directory
/// runs before its count is compared, so a new case is run even while a count
/// here is stale.
const PROFILE_DIRS: [(&str, Profile, usize, usize); 4] = [
    ("provider-attempt", Profile::ProviderAttempt, 100, 83),
    ("provider-streaming", Profile::ProviderStreaming, 2, 7),
    ("provider-token-breakdown", Profile::TokenBreakdown, 42, 14),
    ("gateway-request", Profile::GatewayRequest, 22, 15),
];

/// Fail with every collected problem, so one failing case never hides the
/// cases after it.
fn report(failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{} problems:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// What one valid case shows wrong, if anything.
fn check_valid(registry: &Registry, profile: Profile, fixture: &Json) -> Result<(), String> {
    if text(fixture, "profile") != Some(profile.id()) {
        return Err("the case names another profile".into());
    }
    let projection =
        project_fixture_result(fixture).map_err(|error| format!("rejected with {error}"))?;
    let expected = fixture
        .get("expected_metrics")
        .ok_or("no expected_metrics")
        .and_then(|points| {
            MetricPoint::list_from_json(points).map_err(|_| "bad expected_metrics")
        })?;
    // Points compare as a multiset under the canonical sort.
    let points = sort_metric_points(projection.points);
    let expected = sort_metric_points(expected);
    if points != expected {
        return Err(format!(
            "points differ\n  got:      {points:?}\n  expected: {expected:?}"
        ));
    }
    let issues: BTreeSet<String> = projection
        .issues
        .iter()
        .map(|issue| issue.as_str().to_string())
        .collect();
    let expected_issues = strings(fixture.get("expected_issues"));
    if issues != expected_issues {
        return Err(format!("issues {issues:?}, expected {expected_issues:?}"));
    }
    let resource: Vec<(&str, Option<&str>)> = projection
        .resource
        .iter()
        .map(|(k, v)| (k.as_str(), Some(v.as_str())))
        .collect();
    let expected_resource: Vec<(&str, Option<&str>)> = fixture
        .get("expected_resource_attributes")
        .and_then(Json::as_object)
        .map(|map| map.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect())
        .unwrap_or_default();
    if resource != expected_resource {
        return Err(format!(
            "resource {resource:?}, expected {expected_resource:?}"
        ));
    }
    let findings = registry.check_points(&points);
    if !findings.is_empty() {
        return Err(format!("outside the registry: {findings:?}"));
    }
    let allowed = registry.profile_metrics(profile.id()).unwrap_or_default();
    match points.iter().find(|point| !allowed.contains(&point.name)) {
        Some(point) => Err(format!("{} is not a metric of the profile", point.name)),
        None => Ok(()),
    }
}

#[test]
#[cfg_attr(
    miri,
    ignore = "projects every case; the rule tests cover these paths under Miri"
)]
fn valid_fixtures_project_exactly_and_stay_within_the_registry() {
    let registry = registry();
    let mut failures = Vec::new();
    for (dir, profile, ..) in PROFILE_DIRS {
        for path in fixtures(dir, "valid") {
            if let Err(problem) = check_valid(&registry, profile, &read_json(&path)) {
                failures.push(format!("{}: {problem}", path.display()));
            }
        }
    }
    report(failures);
}

#[test]
#[cfg_attr(
    miri,
    ignore = "projects every case; the rule tests cover these paths under Miri"
)]
fn invalid_fixtures_are_rejected_with_the_expected_error_code() {
    let codes: BTreeSet<&str> = ErrorCode::ALL.iter().map(|code| code.as_str()).collect();
    let mut failures = Vec::new();
    for (dir, ..) in PROFILE_DIRS {
        for path in fixtures(dir, "invalid") {
            let name = path.display();
            let fixture = read_json(&path);
            let expected = text(&fixture, "expected_error_code").unwrap_or("(none stated)");
            if !codes.contains(expected) {
                failures.push(format!("{name}: {expected} is not in the closed list"));
            }
            match project_fixture_result(&fixture) {
                Ok(_) => failures.push(format!("{name}: accepted, expected {expected}")),
                Err(error) if error.code().as_str() != expected => {
                    failures.push(format!("{name}: {}, expected {expected}", error.code()));
                }
                Err(_) => {}
            }
        }
    }
    report(failures);
}

#[test]
fn the_corpus_has_the_stated_number_of_cases() {
    let mut total = 0;
    for (dir, _, valid, invalid) in PROFILE_DIRS {
        assert_eq!(fixtures(dir, "valid").len(), valid, "{dir} valid cases");
        assert_eq!(
            fixtures(dir, "invalid").len(),
            invalid,
            "{dir} invalid cases"
        );
        total += valid + invalid;
    }
    assert_eq!(total, 285);
}

#[test]
fn issue_codes_in_the_cases_are_known_ones() {
    let known: BTreeSet<String> = IssueCode::ALL
        .iter()
        .map(|code| code.as_str().to_string())
        .collect();
    let mut seen = BTreeSet::new();
    for (dir, ..) in PROFILE_DIRS {
        for path in fixtures(dir, "valid") {
            seen.extend(strings(read_json(&path).get("expected_issues")));
        }
    }
    let unknown: Vec<&String> = seen.difference(&known).collect();
    assert!(unknown.is_empty(), "{unknown:?}");
}

#[test]
fn cases_that_hold_a_number_no_double_holds_are_read_from_their_text() {
    let mut found = 0;
    for (dir, ..) in PROFILE_DIRS {
        for kind in ["valid", "invalid"] {
            for path in fixtures(dir, kind) {
                let raw = read_text(&path);
                if !raw.contains("1e400") {
                    continue;
                }
                // The crate's reader takes the token; whether the case then
                // projects is checked with every other case.
                let fixture =
                    Json::parse(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                assert!(holds_infinity(&fixture), "{}", path.display());
                found += 1;
            }
        }
    }
    assert!(found > 0);
}

fn holds_infinity(value: &Json) -> bool {
    match value {
        Json::Number(number) => number.is_infinite(),
        Json::Array(items) => items.iter().any(holds_infinity),
        Json::Object(members) => members.values().any(holds_infinity),
        _ => false,
    }
}

#[test]
fn every_profile_this_crate_claims_is_in_the_registry_at_its_version() {
    let registry = registry();
    let findings = registry.validate();
    assert!(findings.is_empty(), "{findings:?}");
    assert_eq!(registry.registry_version, "0.1.0");
    let ids: BTreeSet<String> = registry.profiles.iter().map(|p| p.identifier()).collect();
    for profile in Profile::ALL {
        assert!(
            ids.contains(profile.id()),
            "{} is not registered",
            profile.id()
        );
    }
}

#[test]
fn the_deployment_attribute_list_is_the_registry_list() {
    let from_registry: BTreeSet<(String, String)> = registry()
        .deployment_attributes
        .into_iter()
        .map(|attribute| (attribute.name, attribute.carrier))
        .collect();
    let built_in: BTreeSet<(String, String)> = DEPLOYMENT_ATTRIBUTES
        .iter()
        .map(|(name, carrier)| (name.to_string(), carrier.as_str().to_string()))
        .collect();
    assert_eq!(built_in, from_registry);
}

#[test]
fn point_check_rejects_attributes_outside_the_allowlist() {
    let registry = registry();
    let fixture = read_json(&data().join("provider-attempt/valid/success-openai.json"));
    let mut points = project_fixture_result(&fixture).unwrap().points;
    let name = points[0].name.clone();
    let attributes = &mut points[0].attributes;
    attributes.insert("session.id".into(), "abc".into());
    attributes.remove("gen_ai.provider.name");
    attributes.insert("gen_ai.operation.name".into(), "summarize".into());
    // A point-carried deployment attribute and the point's own profile are allowed.
    attributes.insert("user.id".into(), "u-1".into());
    attributes.insert(
        "trajectory.profile".into(),
        registry.profile_for_metric(&name).unwrap(),
    );
    // A resource-carried deployment attribute never goes on a point.
    points[1]
        .attributes
        .insert("host.name".into(), "build-17".into());
    points[2].attributes.insert(
        "trajectory.profile".into(),
        Profile::GatewayRequest.id().to_string(),
    );
    let findings: BTreeSet<(FindingCode, String)> = registry
        .check_points(&points)
        .into_iter()
        .map(|finding| (finding.code, finding.subject))
        .collect();
    let expected: BTreeSet<(FindingCode, String)> = [
        (FindingCode::AttributeNotAllowed, "session.id"),
        (
            FindingCode::RequiredAttributeMissing,
            "gen_ai.provider.name",
        ),
        (
            FindingCode::AttributeValueNotAllowed,
            "gen_ai.operation.name",
        ),
        (FindingCode::AttributeNotAllowed, "host.name"),
        (FindingCode::ProfileAttributeMismatch, "trajectory.profile"),
    ]
    .into_iter()
    .map(|(code, subject)| (code, subject.to_string()))
    .collect();
    assert_eq!(findings, expected);
}

#[test]
fn counters_are_aggregated_per_attribute_set() {
    let fixture = read_json(&data().join("gateway-request/valid/cache-miss-then-write.json"));
    let points = project_fixture_result(&fixture).unwrap().points;
    let counters: Vec<&MetricPoint> = points
        .iter()
        .filter(|p| p.name == "trajectory.gen_ai.gateway.cache.operations")
        .collect();
    let keys: BTreeSet<_> = counters.iter().map(|p| &p.attributes).collect();
    assert_eq!(
        keys.len(),
        counters.len(),
        "one counter point per attribute set"
    );
}

/// Adding a request to a batch projects it into the batch's series without
/// a `Projection`: every case gives the same points, issues and error code
/// both ways.
#[test]
#[cfg_attr(
    miri,
    ignore = "projects every case; the rule tests cover these paths under Miri"
)]
fn adding_a_request_matches_adding_its_projection() {
    use libdd_ai_usage::{
        Document, GatewayRequestObservation, MetricBatch, Observation, ProviderAttemptObservation,
        ProviderStreamingObservation, TokenBreakdownObservation,
    };
    let mut failures = Vec::new();
    for (dir, profile, ..) in PROFILE_DIRS {
        for kind in ["valid", "invalid"] {
            for path in fixtures(dir, kind) {
                let name = path.display().to_string();
                let fixture = read_json(&path);
                let input = fixture.get("observation").unwrap_or(&Json::Null);
                let mut old = MetricBatch::new();
                let expected = project_fixture_result(&fixture).map(|projection| {
                    old.try_add(profile, &projection).unwrap();
                    projection.issues
                });
                let deployment: Vec<(&str, Option<&str>)> =
                    match fixture.get("deployment_attributes") {
                        Some(Json::Object(members)) => members
                            .iter()
                            .filter(|(_, value)| !value.is_null())
                            .map(|(key, value)| (key.as_str(), value.as_str()))
                            .collect(),
                        _ => Vec::new(),
                    };
                let decoded = match profile {
                    Profile::ProviderAttempt => ProviderAttemptObservation::from_json(input)
                        .map(|o| (Some(o), None, None, None)),
                    Profile::ProviderStreaming => ProviderStreamingObservation::from_json(input)
                        .map(|o| (None, Some(o), None, None)),
                    Profile::TokenBreakdown => TokenBreakdownObservation::from_json(input)
                        .map(|o| (None, None, Some(o), None)),
                    Profile::GatewayRequest => GatewayRequestObservation::from_json(input)
                        .map(|o| (None, None, None, Some(o))),
                };
                let mut new = MetricBatch::new();
                let got = decoded.and_then(|(a, s, b, g)| {
                    let observation = match (&a, &s, &b, &g) {
                        (Some(o), ..) => Observation::ProviderAttempt(o),
                        (_, Some(o), ..) => Observation::ProviderStreaming(o),
                        (_, _, Some(o), _) => Observation::TokenBreakdown(o),
                        (.., Some(o)) => Observation::GatewayRequest(o),
                        _ => unreachable!(),
                    };
                    new.try_add_request(&[Document {
                        observation,
                        deployment_attributes: &deployment,
                        metrics: None,
                    }])
                    .map(|(_, mut issues)| issues.remove(0))
                    .map_err(|error| error.error)
                });
                match (&expected, &got) {
                    (Ok(a), Ok(b)) if a == b => {
                        if old.points() != new.points() {
                            failures.push(format!("{name}: points differ"));
                        }
                    }
                    (Err(a), Err(b)) if a.code() == b.code() => {}
                    _ => failures.push(format!("{name}: {expected:?} vs {got:?}")),
                }
            }
        }
    }
    report(failures);
}
