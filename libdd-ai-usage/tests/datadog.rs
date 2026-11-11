// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The Datadog binding cases in `tests/data/datadog`: the metric points of one
//! profile for one second, and the exact series an exporter submits for them.
//! Series compare as a multiset; rejections compare by error code.
//!
//! Then the contract between transports: a Counter is a Rate over 10 s on the
//! API, and its Rate times 10 is the total DogStatsD sends, which the Agent
//! stores as the same Rate.

use std::fs;
use std::path::{Path, PathBuf};

use libdd_ai_usage::{
    Attributes, DATADOG_RATE_INTERVAL_SECS, Json, MetricBatch, MetricPoint, Profile, SeriesType,
    datadog_series, project_result,
};

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

/// Name, type, interval, tags and value.
type Series = (String, String, Option<u32>, Vec<String>, f64);

fn sort(series: &mut [Series]) {
    series.sort_by(|a, b| {
        (&a.0, &a.3)
            .cmp(&(&b.0, &b.3))
            .then_with(|| a.4.total_cmp(&b.4))
            .then_with(|| a.1.cmp(&b.1))
    });
}

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
                s.get("interval")
                    .and_then(Json::as_f64)
                    .map(|interval| interval as u32),
                tags,
                s.get("value").and_then(Json::as_f64).unwrap(),
            )
        })
        .collect();
    sort(&mut series);
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
                        .map(|s| {
                            (
                                s.name,
                                s.series_type.as_str().to_string(),
                                s.interval,
                                s.tags,
                                s.value,
                            )
                        })
                        .collect();
                    sort(&mut got);
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

fn attempt(input_tokens: u64, cost_usd: &str, duration: &str) -> libdd_ai_usage::Projection {
    let observation = Json::parse(&format!(
        r#"{{"operation_name": "chat", "provider_name": "openai", "duration_seconds": {duration},
            "streaming": false, "input_tokens": {input_tokens}, "input_basis": "includes_cache",
            "output_tokens": 7, "output_basis": "includes_reasoning",
            "cost_usd": {cost_usd}, "cost_source": "reported",
            "observation_point": "gateway"}}"#
    ))
    .unwrap();
    project_result(Profile::ProviderAttempt, &observation).unwrap()
}

/// A batch of a few calls, with costs whose USD totals are not exact
/// doubles.
fn batch() -> MetricBatch {
    let mut batch = MetricBatch::new();
    for (tokens, cost, duration) in [
        (100, "0.1", "0.2"),
        (3, "0.2", "0.3"),
        (9_007_199_254_740_000, "0.0000000021", "0.4"),
        (100, "0.000000015", "1.5"),
    ] {
        batch
            .add(Profile::ProviderAttempt, &attempt(tokens, cost, duration))
            .unwrap();
    }
    batch
}

/// A DogStatsD line's name, type and tags.
type LineKey = (String, String, Vec<String>);

/// The DogStatsD lines as their key and value.
fn dogstatsd(batch: &MetricBatch) -> Vec<(LineKey, f64)> {
    batch
        .dogstatsd_lines()
        .unwrap()
        .iter()
        .map(|line| {
            let mut parts = line.split('|');
            let (name, value) = parts.next().unwrap().split_once(':').unwrap();
            let kind = parts.next().unwrap().to_string();
            let tags = parts
                .next()
                .map(|t| {
                    t.trim_start_matches('#')
                        .split(',')
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            ((name.to_string(), kind, tags), value.parse().unwrap())
        })
        .collect()
}

#[test]
fn every_api_counter_is_a_rate_over_ten_seconds() {
    assert_eq!(DATADOG_RATE_INTERVAL_SECS, 10);
    let series = batch().datadog_series(false).unwrap();
    assert!(series.iter().any(|s| s.series_type == SeriesType::Rate));
    assert!(
        series
            .iter()
            .any(|s| s.series_type == SeriesType::Distribution)
    );
    for s in &series {
        match s.series_type {
            SeriesType::Rate => {
                assert_eq!(s.series_type.as_str(), "rate");
                assert_eq!(s.interval, Some(10), "{s:?}");
            }
            SeriesType::Distribution => {
                assert_eq!(s.series_type.as_str(), "distribution");
                assert_eq!(s.interval, None, "{s:?}");
                assert!(!s.name.ends_with(".sum") && !s.name.ends_with(".count"));
            }
        }
    }
    // Historical submissions are Rates only.
    let historical = batch().datadog_series(true).unwrap();
    assert!(!historical.is_empty());
    assert!(
        historical
            .iter()
            .all(|s| s.series_type == SeriesType::Rate && s.interval == Some(10))
    );
}

#[test]
fn an_api_rate_times_its_interval_is_the_dogstatsd_total() {
    let batch = batch();
    let lines = dogstatsd(&batch);
    let series = batch.datadog_series(false).unwrap();
    let mut rates = 0;
    for s in series.iter().filter(|s| s.series_type == SeriesType::Rate) {
        let key = (s.name.clone(), "c".to_string(), s.tags.clone());
        let matching: Vec<f64> = lines
            .iter()
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .collect();
        assert_eq!(matching.len(), 1, "{key:?}");
        let total = matching[0];
        let interval = f64::from(s.interval.unwrap());
        let whole = s.name.ends_with("_tokens") || s.name.ends_with(".count");
        if whole && total < 2f64.powi(50) {
            // These whole counts round-trip exactly.
            assert_eq!(s.value * interval, total, "{key:?}");
        } else {
            // USD and fractional sums, and counts near 2^53: the Rate is one
            // division of the exact sum, so value * 10 is the total to
            // within the last bit.
            assert!(
                (s.value * interval - total).abs() <= total.abs() * f64::EPSILON,
                "{key:?}: {} * 10 != {total}",
                s.value
            );
        }
        rates += 1;
    }
    // Every DogStatsD count has its Rate, and every sample its Distribution.
    let counts = lines.iter().filter(|((_, kind, _), _)| kind == "c").count();
    assert_eq!(rates, counts);
    let samples = lines.iter().filter(|((_, kind, _), _)| kind == "d").count();
    assert_eq!(
        samples,
        series
            .iter()
            .filter(|s| s.series_type == SeriesType::Distribution)
            .count()
    );
    // The take for the API intake path submits the same Rates.
    let mut taken = self::batch();
    let intake = taken.take_intake(1).unwrap();
    assert_eq!(intake.interval, 10);
    assert_eq!(intake.rates.len(), rates);
    for rate in &intake.rates {
        let s = series
            .iter()
            .find(|s| s.name == rate.metric && s.tags == rate.tags)
            .unwrap();
        assert_eq!(rate.value, s.value, "{}", rate.metric);
    }
}

#[test]
fn the_same_tokens_over_both_transports_read_as_one_total() {
    // 100 tokens over DogStatsD and 100 over the API in one 10 s bucket: both
    // land as a Rate of 10 over 10 s, so as_count() reads 200.
    let mut batch = MetricBatch::new();
    batch
        .add(Profile::ProviderAttempt, &attempt(100, "0.1", "0.2"))
        .unwrap();
    let name = "gen_ai.client.inference.usage.input_tokens";
    let dogstatsd_total = dogstatsd(&batch)
        .into_iter()
        .find(|((n, kind, _), _)| n == name && kind == "c")
        .unwrap()
        .1;
    // The Agent stores a DogStatsD count as count / 10 over 10 s.
    let agent_rate = dogstatsd_total / 10.0;
    let api = batch
        .datadog_series(false)
        .unwrap()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap();
    assert_eq!((api.value, api.interval), (agent_rate, Some(10)));
    assert_eq!((api.value + agent_rate) * 10.0, 200.0);
}
