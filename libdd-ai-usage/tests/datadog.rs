// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The Datadog binding cases in `tests/data/datadog`: the metric points of one
//! profile for one second, and the exact series an exporter submits for them.
//! Series compare as a multiset; rejections compare by error code.

use std::fs;
use std::path::{Path, PathBuf};

use libdd_ai_usage::{Attributes, Json, MetricPoint, Profile, datadog_series};

fn cases() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/datadog");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    files
}

type Series = (String, String, Vec<String>, f64);

fn expected_series(case: &Json) -> Vec<Series> {
    let mut series: Vec<Series> = case
        .get("expected_series")
        .and_then(Json::as_array)
        .unwrap()
        .iter()
        .map(|s| {
            let text = |key: &str| s.get(key).and_then(Json::as_str).unwrap().to_string();
            let tags = s
                .get("tags")
                .and_then(Json::as_array)
                .unwrap()
                .iter()
                .map(|t| t.as_str().unwrap().to_string())
                .collect();
            (
                text("name"),
                text("type"),
                tags,
                s.get("value").and_then(Json::as_f64).unwrap(),
            )
        })
        .collect();
    series.sort_by(|a, b| {
        (&a.0, &a.2)
            .cmp(&(&b.0, &b.2))
            .then_with(|| a.3.total_cmp(&b.3))
            .then_with(|| a.1.cmp(&b.1))
    });
    series
}

#[test]
fn binding_cases_give_the_expected_series() {
    let mut failures = Vec::new();
    let paths = cases();
    assert_eq!(paths.len(), 16);
    for path in paths {
        let name = path.display().to_string();
        let case = Json::parse(&fs::read_to_string(&path).unwrap()).unwrap();
        let id = case.get("profile").and_then(Json::as_str).unwrap();
        let profile = Profile::from_id(id).unwrap_or_else(|| panic!("{name}: {id}"));
        let points = MetricPoint::list_from_json(case.get("points").unwrap()).unwrap();
        let resource: Attributes = case
            .get("resource_attributes")
            .and_then(Json::as_object)
            .map(|map| {
                map.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let historical = case.get("historical").and_then(Json::as_bool) == Some(true);
        let result = datadog_series(profile, &points, &resource, historical);
        match case.get("expected_error_code").and_then(Json::as_str) {
            Some(code) => match result {
                Err(error) if error.code().as_str() == code => {}
                other => failures.push(format!("{name}: {other:?}, expected {code}")),
            },
            None => match result {
                Err(error) => failures.push(format!("{name}: rejected with {error}")),
                Ok(series) => {
                    let mut got: Vec<Series> = series
                        .into_iter()
                        .map(|s| (s.name, s.series_type.as_str().to_string(), s.tags, s.value))
                        .collect();
                    got.sort_by(|a, b| {
                        (&a.0, &a.2)
                            .cmp(&(&b.0, &b.2))
                            .then_with(|| a.3.total_cmp(&b.3))
                            .then_with(|| a.1.cmp(&b.1))
                    });
                    let expected = expected_series(&case);
                    if got != expected {
                        failures.push(format!(
                            "{name}:\n  got:      {got:?}\n  expected: {expected:?}"
                        ));
                    }
                }
            },
        }
    }
    assert!(
        failures.is_empty(),
        "{} problems:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn a_metric_of_another_profile_is_rejected() {
    let point = MetricPoint::counter(
        "trajectory.gen_ai.gateway.request.estimated_cost",
        "{nanoUSD}",
        1.0,
        Attributes::new(),
    );
    let error = datadog_series(
        Profile::ProviderAttempt,
        &[point],
        &Attributes::new(),
        false,
    )
    .unwrap_err();
    assert_eq!(error.code().as_str(), "metric_profile_mismatch");
}
