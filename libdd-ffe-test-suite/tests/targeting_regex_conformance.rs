// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{collections::HashSet, fs, path::PathBuf};

use libdd_common::regex_engine::Regex;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConformanceFixture {
    schema: String,
    schema_version: u64,
    contract_version: String,
    cases: Vec<ConformanceCase>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConformanceCase {
    id: String,
    raw_pattern: String,
    input: String,
    expected_compile: Option<bool>,
    expected_match: Option<bool>,
    #[serde(default)]
    engine_expectations: EngineExpectations,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EngineExpectations {
    rust_rules_based: Option<RegexExpectation>,
}

#[derive(Debug, Deserialize)]
struct RegexExpectation {
    compile: bool,
    #[serde(rename = "match")]
    matches: Option<bool>,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("ffe-system-test-data/regex-conformance/targeting-regex-conformance.json")
}

#[test]
fn evaluates_targeting_regex_conformance_fixture() {
    let path = fixture_path();
    assert!(
        path.exists(),
        "targeting regex conformance fixture is missing at {}; initialize the submodule",
        path.display()
    );

    let fixture: ConformanceFixture = serde_json::from_reader(fs::File::open(&path).unwrap())
        .unwrap_or_else(|err| panic!("failed to parse fixture {}: {err}", path.display()));

    assert_eq!(fixture.schema, "datadog.ffe.targeting-regex-conformance/v1");
    assert_eq!(fixture.schema_version, 1);
    assert_eq!(fixture.contract_version, "targeting-regex-v1");
    assert_eq!(fixture.cases.len(), 75, "unexpected fixture case count");
    let unique_ids: HashSet<_> = fixture.cases.iter().map(|case| &case.id).collect();
    assert_eq!(unique_ids.len(), 75, "fixture case IDs must be unique");

    let mut mismatches = Vec::new();
    for case in fixture.cases {
        let (expected_compile, expected_match) = case
            .engine_expectations
            .rust_rules_based
            .map(|expectation| (Some(expectation.compile), expectation.matches))
            .unwrap_or((case.expected_compile, case.expected_match));
        let expected_compile = expected_compile
            .unwrap_or_else(|| panic!("{} has no Rust compile expectation", case.id));

        let compiled = Regex::new(&case.raw_pattern);
        let actual_compile = compiled.is_ok();
        if actual_compile != expected_compile {
            mismatches.push(format!(
                "{}: compile expected {expected_compile}, got {actual_compile} ({:?})",
                case.id, case.raw_pattern
            ));
            continue;
        }

        let actual_match = compiled
            .as_ref()
            .is_ok_and(|regex| regex.is_match(&case.input));
        let expected_match = expected_match.unwrap_or(false);
        if actual_match != expected_match {
            mismatches.push(format!(
                "{}: match expected {expected_match}, got {actual_match} ({:?} against {:?})",
                case.id, case.raw_pattern, case.input
            ));
        }
    }

    assert!(
        mismatches.is_empty(),
        "regex conformance mismatches ({}):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
