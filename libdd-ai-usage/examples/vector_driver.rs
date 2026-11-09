// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A vector driver for the Open Trajectory conformance tool
//! (`trajectory-conformance run-vectors --suite metrics --driver <this binary>`).
//! It reads one job per line on standard input and writes one result per line
//! on standard output.
//!
//! A job is read with the crate's own JSON reader, which keeps numbers such as
//! `1e400` that no double holds. A job that is not JSON or has no string `id`
//! gets no result line, so the verifier reports it as unanswered; the reason
//! goes to standard error.
//!
//! cargo build -q -p libdd-ai-usage --example vector_driver

use std::io::{self, BufRead, BufWriter, Write};

use libdd_ai_usage::{Json, project_fixture_result, sort_metric_points};
use serde_json::{Map, Value, json};

fn answer(text: &str) -> Result<Value, String> {
    let job = Json::parse(text).map_err(|error| error.to_string())?;
    let id = job
        .get("id")
        .and_then(Json::as_str)
        .ok_or("the job has no string id")?;
    let mut result = Map::new();
    result.insert("id".into(), json!(id));
    match project_fixture_result(&job) {
        Ok(projection) => {
            result.insert(
                "points".into(),
                json!(sort_metric_points(projection.points)),
            );
            if !projection.issues.is_empty() {
                result.insert("issues".into(), json!(projection.issues));
            }
            if !projection.resource.is_empty() {
                result.insert("resource".into(), json!(projection.resource));
            }
        }
        Err(error) => {
            result.insert("error_code".into(), json!(error.code().as_str()));
            result.insert("error".into(), json!(error.message()));
        }
    }
    Ok(Value::Object(result))
}

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut out = BufWriter::new(io::stdout().lock());
    for (index, line) in stdin.lock().lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match answer(&line) {
            Ok(result) => writeln!(out, "{result}")?,
            Err(reason) => eprintln!("job line {}: not answered: {reason}", index + 1),
        }
    }
    out.flush()
}
