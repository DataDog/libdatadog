// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! JUnit report reading for the crate-timing report: each test case's name,
//! duration and pass/fail status. nextest writes its JUnit reports with
//! quick-junit, so the same crate's deserializer reads them back.

use quick_junit::{Report, TestCaseStatus};

pub struct TestCase {
    pub name: String,
    pub secs: f64,
    pub ok: bool,
}

pub fn parse(xml: &str) -> Vec<TestCase> {
    let Ok(report) = Report::deserialize_from_str(xml) else {
        return Vec::new();
    };
    report
        .test_suites
        .iter()
        .flat_map(|suite| suite.test_cases.iter())
        .map(|case| TestCase {
            name: case.name.as_str().to_owned(),
            secs: case.time.map_or(0.0, |t| t.as_secs_f64()),
            ok: !matches!(case.status, TestCaseStatus::NonSuccess { .. }),
        })
        .filter(|case| !case.name.is_empty())
        .collect()
}
