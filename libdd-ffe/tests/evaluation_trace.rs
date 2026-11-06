// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A separate test binary isolates trace capture from the telemetry unit-test logger.

use std::sync::Mutex;

use chrono::Utc;
use libdd_ffe::rules_based::{
    Attribute, Configuration, EvaluationContext, EvaluationOptions, ExpectedFlagType,
    ExperimentationOptOutMode, UniversalFlagConfig,
};
use serde_json::{Value, json};

struct TraceLogger(Mutex<Vec<(String, String)>>);

static LOGGER: TraceLogger = TraceLogger(Mutex::new(Vec::new()));

impl log::Log for TraceLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() == log::Level::Trace
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata())
            && record.target() == "libdd_ffe::rules_based::eval::eval_assignment"
        {
            self.0.lock().unwrap().push((
                record.args().to_string(),
                record
                    .key_values()
                    .get("flag_key".into())
                    .unwrap()
                    .to_string(),
            ));
        }
    }

    fn flush(&self) {}
}

fn assert_one_log(flag_key: &str, succeeded: bool) {
    let messages = std::mem::take(&mut *LOGGER.0.lock().unwrap());
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert_eq!(messages[0].1, flag_key);
    let prefix = if succeeded {
        "evaluated a flag"
    } else {
        "returning default assignment because of:"
    };
    assert!(messages[0].0.starts_with(prefix), "{messages:?}");
}

fn flag(conditions: Vec<Value>) -> Value {
    json!({
        "key": "ignored", "enabled": true, "variationType": "BOOLEAN",
        "variations": {"on": {"key": "on", "value": true}},
        "allocations": [{
            "key": "allocation", "doLog": true,
            "rules": [{"conditions": conditions}],
            "splits": [{"variationKey": "on", "shards": []}]
        }]
    })
}

#[test]
fn trace_and_assignment_apis_log_each_root_once_including_early_errors() {
    log::set_logger(&LOGGER).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let dependency =
        |key: &str| json!({"flagEvaluation": {"key": key}, "operator": "ONE_OF", "value": ["on"]});
    let config = Configuration::from_server_response(
        UniversalFlagConfig::from_json(
            serde_json::to_vec(&json!({
                "createdAt": "2026-10-28T00:00:00Z",
                "environment": {"name": "test"},
                "flags": {
                    "independent": flag(vec![]),
                    "dependent": flag(vec![dependency("independent")]),
                    "failed-dependency": flag(vec![dependency("missing")])
                }
            }))
            .unwrap(),
        )
        .unwrap(),
    );
    let context = EvaluationContext::new(Some("subject".into()), Default::default());
    for (key, succeeded) in [
        ("independent", true),
        ("dependent", true),
        ("missing", false),
        ("failed-dependency", false),
    ] {
        let trace = config.eval_flag_with_trace(
            key,
            &context,
            ExpectedFlagType::Boolean,
            Utc::now(),
            EvaluationOptions::default(),
        );
        assert_eq!(trace.result.is_ok(), succeeded);
        assert_one_log(key, succeeded);

        let assignment = config.eval_flag_with_options(
            key,
            &context,
            ExpectedFlagType::Boolean,
            Utc::now(),
            EvaluationOptions::default(),
        );
        assert_eq!(assignment.is_ok(), succeeded);
        assert_one_log(key, succeeded);
    }

    let invalid_context = EvaluationContext::new(
        Some("subject".into()),
        std::sync::Arc::new(
            [(
                "dd_experimentation_opt_out_at".into(),
                Attribute::from(true),
            )]
            .into_iter()
            .collect(),
        ),
    );
    let trace = config.eval_flag_with_trace(
        "independent",
        &invalid_context,
        ExpectedFlagType::Boolean,
        Utc::now(),
        EvaluationOptions {
            experimentation_opt_out_mode: ExperimentationOptOutMode::Immediate,
        },
    );
    assert!(trace.result.is_err());
    assert_one_log("independent", false);
}
