// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// Minimal demo of how a `HealthReport` is shipped to Datadog via the local
// trace-agent's evp_proxy. Build with:
//   cargo build -p libdd-healthplatform --example evp_proxy_send \
//       --features example-client,decode
//
// Wire format
// -----------
// The datadog-agent's healthplatform forwarder (comp/healthplatform/impl/forwarder.go)
// posts the report as JSON with Content-Type: application/json — NOT protobuf.
// The agthealth-worker dispatches on `event.textFormat`: only `INTERNAL_INTAKE_REQUEST`
// (a batched protobuf format) or JSON-text are accepted; anything else increments
// `dd.evp_worker.agthealth.events.dropped_invalid_text_format` and the event is
// dropped silently (the EVP intake still returns 202).
//
// This example therefore serialises the in-memory `HealthReport` to JSON with
// snake_case keys matching the protoc-gen-go JSON tags used by the agent.

use anyhow::{anyhow, Result};
use libdd_healthplatform::{
    HealthReport, HostInfo, Issue, IssueState, PersistedIssue, Remediation, RemediationStep,
};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

// Defaults target the per-track agenthealth intake:
//   https://agenthealth-intake.<site>/api/v2/agenthealth
const DEFAULT_TRACE_AGENT_URL: &str = "http://localhost:8136";
const DEFAULT_EVP_PROXY_PATH: &str = "/evp_proxy/v4/api/v2/agenthealth";
const DEFAULT_EVP_PROXY_SUBDOMAIN: &str = "agenthealth-intake";

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut days = (secs / 86_400) as i64;
    let mut sod = secs % 86_400;
    let hh = sod / 3600;
    sod %= 3600;
    let mm = sod / 60;
    let ss = sod % 60;
    let mut year = 1970i64;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
        let yd = if leap { 366 } else { 365 };
        if days < yd {
            break;
        }
        days -= yd;
        year += 1;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let mdays = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0usize;
    while month < 12 && days >= mdays[month] {
        days -= mdays[month];
        month += 1;
    }
    let day = days + 1;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month + 1,
        day,
        hh,
        mm,
        ss
    )
}

fn sample_report() -> HealthReport {
    let now = now_rfc3339();

    let issue = Issue {
        id: "postgres_connectivity:instance-1".into(),
        issue_name: "postgres_connectivity".into(),
        title: "Postgres check cannot reach instance-1".into(),
        description: "The postgres integration failed to connect to host db-1:5432 — \
                      connection refused. The check has been failing for 3 consecutive runs."
            .into(),
        category: "connectivity".into(),
        location: "core-agent".into(),
        severity: "high".into(),
        detected_at: now.clone(),
        source: "datadog-agent".into(),
        extra: None,
        remediation: Some(Remediation {
            summary: "Confirm the postgres instance is reachable and credentials are valid.".into(),
            steps: vec![
                RemediationStep {
                    order: 1,
                    text: "Verify the postgres process is running and listening on the configured port.".into(),
                },
                RemediationStep {
                    order: 2,
                    text: "From the agent host run `psql -h db-1 -p 5432 -U datadog` and check it succeeds.".into(),
                },
                RemediationStep {
                    order: 3,
                    text: "If the connection still fails, review firewall / security group rules between the agent host and the database.".into(),
                },
            ],
            script: None,
        }),
        tags: vec![
            "env:staging".into(),
            "team:libdatadog".into(),
            "integration:postgres".into(),
            "source:libdd-healthplatform-demo".into(),
        ],
        persisted_issue: Some(PersistedIssue {
            state: IssueState::New as i32,
            first_seen: now.clone(),
            last_seen: now.clone(),
            resolved_at: None,
        }),
    };

    let mut issues = BTreeMap::new();
    issues.insert("postgres_connectivity:instance-1".into(), issue);

    HealthReport {
        schema_version: "1".into(),
        event_type: "agent_health".into(),
        emitted_at: now,
        host: Some(HostInfo {
            hostname: "libdd-healthplatform-demo".into(),
            agent_version: Some("7.77.3".into()),
            par_ids: vec![],
        }),
        issues,
        service: "datadog-agent".into(),
    }
}

/// Serialise a `HealthReport` to JSON with snake_case keys, matching the agent
/// forwarder (`encoding/json` on protoc-gen-go structs). Empty/None fields are
/// omitted to mirror Go's `omitempty` behaviour.
fn health_report_to_json(report: &HealthReport) -> Value {
    let mut obj = Map::new();
    if !report.schema_version.is_empty() {
        obj.insert(
            "schema_version".into(),
            Value::String(report.schema_version.clone()),
        );
    }
    if !report.event_type.is_empty() {
        obj.insert(
            "event_type".into(),
            Value::String(report.event_type.clone()),
        );
    }
    if !report.emitted_at.is_empty() {
        obj.insert(
            "emitted_at".into(),
            Value::String(report.emitted_at.clone()),
        );
    }
    if let Some(host) = &report.host {
        let mut h = Map::new();
        if !host.hostname.is_empty() {
            h.insert("hostname".into(), Value::String(host.hostname.clone()));
        }
        if let Some(v) = &host.agent_version {
            h.insert("agent_version".into(), Value::String(v.clone()));
        }
        if !host.par_ids.is_empty() {
            h.insert(
                "par_ids".into(),
                Value::Array(host.par_ids.iter().cloned().map(Value::String).collect()),
            );
        }
        obj.insert("host".into(), Value::Object(h));
    }
    if !report.issues.is_empty() {
        let mut m = Map::new();
        for (k, issue) in &report.issues {
            m.insert(k.clone(), issue_to_json(issue));
        }
        obj.insert("issues".into(), Value::Object(m));
    }
    if !report.service.is_empty() {
        obj.insert("service".into(), Value::String(report.service.clone()));
    }
    Value::Object(obj)
}

fn issue_to_json(issue: &Issue) -> Value {
    let mut o = Map::new();
    if !issue.id.is_empty() {
        o.insert("id".into(), Value::String(issue.id.clone()));
    }
    if !issue.issue_name.is_empty() {
        o.insert("issue_name".into(), Value::String(issue.issue_name.clone()));
    }
    if !issue.title.is_empty() {
        o.insert("title".into(), Value::String(issue.title.clone()));
    }
    if !issue.description.is_empty() {
        o.insert(
            "description".into(),
            Value::String(issue.description.clone()),
        );
    }
    if !issue.category.is_empty() {
        o.insert("category".into(), Value::String(issue.category.clone()));
    }
    if !issue.location.is_empty() {
        o.insert("location".into(), Value::String(issue.location.clone()));
    }
    if !issue.severity.is_empty() {
        o.insert("severity".into(), Value::String(issue.severity.clone()));
    }
    if !issue.detected_at.is_empty() {
        o.insert(
            "detected_at".into(),
            Value::String(issue.detected_at.clone()),
        );
    }
    if !issue.source.is_empty() {
        o.insert("source".into(), Value::String(issue.source.clone()));
    }
    if let Some(rem) = &issue.remediation {
        let mut r = Map::new();
        if !rem.summary.is_empty() {
            r.insert("summary".into(), Value::String(rem.summary.clone()));
        }
        if !rem.steps.is_empty() {
            let steps: Vec<Value> = rem
                .steps
                .iter()
                .map(|s| json!({ "order": s.order, "text": s.text }))
                .collect();
            r.insert("steps".into(), Value::Array(steps));
        }
        if let Some(script) = &rem.script {
            let mut s = Map::new();
            if !script.language.is_empty() {
                s.insert("language".into(), Value::String(script.language.clone()));
            }
            if !script.language_version.is_empty() {
                s.insert(
                    "language_version".into(),
                    Value::String(script.language_version.clone()),
                );
            }
            if !script.filename.is_empty() {
                s.insert("filename".into(), Value::String(script.filename.clone()));
            }
            if script.requires_root {
                s.insert("requires_root".into(), Value::Bool(true));
            }
            if !script.content.is_empty() {
                s.insert("content".into(), Value::String(script.content.clone()));
            }
            r.insert("script".into(), Value::Object(s));
        }
        o.insert("remediation".into(), Value::Object(r));
    }
    if !issue.tags.is_empty() {
        o.insert(
            "tags".into(),
            Value::Array(issue.tags.iter().cloned().map(Value::String).collect()),
        );
    }
    if let Some(pi) = &issue.persisted_issue {
        let mut p = Map::new();
        // protoc-gen-go serialises proto enums as their integer value by default.
        p.insert("state".into(), Value::Number(pi.state.into()));
        if !pi.first_seen.is_empty() {
            p.insert("first_seen".into(), Value::String(pi.first_seen.clone()));
        }
        if !pi.last_seen.is_empty() {
            p.insert("last_seen".into(), Value::String(pi.last_seen.clone()));
        }
        if let Some(v) = &pi.resolved_at {
            p.insert("resolved_at".into(), Value::String(v.clone()));
        }
        o.insert("persisted_issue".into(), Value::Object(p));
    }
    Value::Object(o)
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("evp_proxy_send: {err:?}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let trace_agent_url = env_or("DD_TRACE_AGENT_URL", DEFAULT_TRACE_AGENT_URL);
    let evp_path = env_or("DD_EVP_PROXY_PATH", DEFAULT_EVP_PROXY_PATH);
    let subdomain = env_or("DD_EVP_PROXY_SUBDOMAIN", DEFAULT_EVP_PROXY_SUBDOMAIN);

    let report = sample_report();
    let body = serde_json::to_vec(&health_report_to_json(&report))?;

    let url = format!("{trace_agent_url}{evp_path}");
    println!(
        "evp_proxy_send: POST {url} (subdomain={subdomain}, {} bytes JSON)",
        body.len()
    );
    let response = reqwest::Client::new()
        .post(&url)
        .header("Content-Type", "application/json")
        .header("X-Datadog-EVP-Subdomain", &subdomain)
        // DD-API-KEY is required by evp_proxy validation; the trace-agent
        // overwrites it with its configured key before forwarding upstream.
        .header("DD-API-KEY", "dummy")
        .body(body)
        .send()
        .await?;

    let status = response.status();
    let bytes = response.bytes().await?;

    if !status.is_success() {
        let preview = String::from_utf8_lossy(&bytes);
        return Err(anyhow!(
            "non-2xx response from {url}: {status} body: {preview}"
        ));
    }

    println!("evp_proxy_send: {status} ({} bytes received)", bytes.len());
    Ok(())
}
