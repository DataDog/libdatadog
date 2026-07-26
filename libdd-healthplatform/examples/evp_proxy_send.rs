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

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

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
    while days >= if is_leap_year(year) { 366 } else { 365 } {
        days -= if is_leap_year(year) { 366 } else { 365 };
        year += 1;
    }
    let feb = if is_leap_year(year) { 29 } else { 28 };
    let mdays = [31, feb, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 0usize;
    while month < 12 && days >= mdays[month] {
        days -= mdays[month];
        month += 1;
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month + 1,
        days + 1,
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

// Mirror Go's `encoding/json` + `omitempty`: empty strings, empty slices, and
// None options are elided from the output.
fn put_str(m: &mut Map<String, Value>, key: &str, s: &str) {
    if !s.is_empty() {
        m.insert(key.into(), Value::String(s.into()));
    }
}

fn put_opt_str(m: &mut Map<String, Value>, key: &str, s: &Option<String>) {
    if let Some(v) = s {
        m.insert(key.into(), Value::String(v.clone()));
    }
}

fn put_str_vec(m: &mut Map<String, Value>, key: &str, v: &[String]) {
    if !v.is_empty() {
        m.insert(
            key.into(),
            Value::Array(v.iter().cloned().map(Value::String).collect()),
        );
    }
}

/// Serialise a `HealthReport` to JSON with snake_case keys, matching the agent
/// forwarder (`encoding/json` on protoc-gen-go structs).
fn health_report_to_json(report: &HealthReport) -> Value {
    let mut o = Map::new();
    put_str(&mut o, "schema_version", &report.schema_version);
    put_str(&mut o, "event_type", &report.event_type);
    put_str(&mut o, "emitted_at", &report.emitted_at);
    if let Some(host) = &report.host {
        let mut h = Map::new();
        put_str(&mut h, "hostname", &host.hostname);
        put_opt_str(&mut h, "agent_version", &host.agent_version);
        put_str_vec(&mut h, "par_ids", &host.par_ids);
        o.insert("host".into(), Value::Object(h));
    }
    if !report.issues.is_empty() {
        let mut m = Map::new();
        for (k, issue) in &report.issues {
            m.insert(k.clone(), issue_to_json(issue));
        }
        o.insert("issues".into(), Value::Object(m));
    }
    put_str(&mut o, "service", &report.service);
    Value::Object(o)
}

fn issue_to_json(issue: &Issue) -> Value {
    let mut o = Map::new();
    put_str(&mut o, "id", &issue.id);
    put_str(&mut o, "issue_name", &issue.issue_name);
    put_str(&mut o, "title", &issue.title);
    put_str(&mut o, "description", &issue.description);
    put_str(&mut o, "category", &issue.category);
    put_str(&mut o, "location", &issue.location);
    put_str(&mut o, "severity", &issue.severity);
    put_str(&mut o, "detected_at", &issue.detected_at);
    put_str(&mut o, "source", &issue.source);
    if let Some(rem) = &issue.remediation {
        let mut r = Map::new();
        put_str(&mut r, "summary", &rem.summary);
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
            put_str(&mut s, "language", &script.language);
            put_str(&mut s, "language_version", &script.language_version);
            put_str(&mut s, "filename", &script.filename);
            if script.requires_root {
                s.insert("requires_root".into(), Value::Bool(true));
            }
            put_str(&mut s, "content", &script.content);
            r.insert("script".into(), Value::Object(s));
        }
        o.insert("remediation".into(), Value::Object(r));
    }
    put_str_vec(&mut o, "tags", &issue.tags);
    if let Some(pi) = &issue.persisted_issue {
        let mut p = Map::new();
        // protoc-gen-go serialises proto enums as their integer value by default.
        p.insert("state".into(), Value::Number(pi.state.into()));
        put_str(&mut p, "first_seen", &pi.first_seen);
        put_str(&mut p, "last_seen", &pi.last_seen);
        put_opt_str(&mut p, "resolved_at", &pi.resolved_at);
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
