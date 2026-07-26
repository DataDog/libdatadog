// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_healthplatform::{
    HealthReport, HostInfo, Issue, IssueState, PersistedIssue, Remediation, RemediationStep, Script,
};
use prost::Message;
use prost_types::value::Kind;
use prost_types::{Struct, Value};
use std::collections::BTreeMap;

fn populated_report(state: IssueState) -> HealthReport {
    let mut extra_fields = BTreeMap::new();
    extra_fields.insert(
        "docker_dir".to_string(),
        Value {
            kind: Some(Kind::StringValue("/var/lib/docker".to_string())),
        },
    );
    extra_fields.insert(
        "retries".to_string(),
        Value {
            kind: Some(Kind::NumberValue(3.0)),
        },
    );

    let issue = Issue {
        id: "abc-123".to_string(),
        issue_name: "docker-permissions-denied".to_string(),
        title: "Docker socket not readable".to_string(),
        description: "The agent could not read /var/run/docker.sock".to_string(),
        category: "permissions".to_string(),
        location: "core-agent".to_string(),
        severity: "warning".to_string(),
        detected_at: "2026-05-14T00:00:00Z".to_string(),
        source: "logs".to_string(),
        extra: Some(Struct {
            fields: extra_fields,
        }),
        remediation: Some(Remediation {
            summary: "Grant the agent user access to the docker socket".to_string(),
            steps: vec![
                RemediationStep {
                    order: 1,
                    text: "Add the dd-agent user to the docker group".to_string(),
                },
                RemediationStep {
                    order: 2,
                    text: "Restart the agent".to_string(),
                },
            ],
            script: Some(Script {
                language: "bash".to_string(),
                language_version: ">=4".to_string(),
                filename: "fix.sh".to_string(),
                requires_root: true,
                content: "usermod -aG docker dd-agent && systemctl restart datadog-agent"
                    .to_string(),
            }),
        }),
        tags: vec!["env:prod".to_string(), "team:agent".to_string()],
        persisted_issue: Some(PersistedIssue {
            state: state as i32,
            first_seen: "2026-05-13T23:00:00Z".to_string(),
            last_seen: "2026-05-14T00:00:00Z".to_string(),
            resolved_at: if state == IssueState::Resolved {
                Some("2026-05-14T00:05:00Z".to_string())
            } else {
                None
            },
        }),
    };

    let mut issues = BTreeMap::new();
    issues.insert("docker-permissions".to_string(), issue);

    HealthReport {
        schema_version: "1".to_string(),
        event_type: "agent_health".to_string(),
        emitted_at: "2026-05-14T00:00:00Z".to_string(),
        host: Some(HostInfo {
            hostname: "demo-host".to_string(),
            agent_version: Some("7.99.0".to_string()),
            par_ids: vec!["org-1".to_string(), "org-2".to_string()],
        }),
        issues,
        service: "datadog-agent".to_string(),
    }
}

#[test]
fn populated_report_roundtrips() {
    let report = populated_report(IssueState::Ongoing);
    let bytes = report.encode_to_vec();
    assert!(!bytes.is_empty(), "encoded report should not be empty");

    let decoded = HealthReport::decode(bytes.as_slice()).expect("decode succeeds");
    assert_eq!(decoded, report);
}

#[test]
fn empty_report_roundtrips_to_default() {
    let report = HealthReport::default();
    let bytes = report.encode_to_vec();
    assert!(
        bytes.is_empty(),
        "default report should encode to zero bytes"
    );

    let decoded = HealthReport::decode(bytes.as_slice()).expect("decode succeeds");
    assert_eq!(decoded, HealthReport::default());
}

#[test]
fn all_issue_states_roundtrip() {
    for state in [
        IssueState::Unspecified,
        IssueState::New,
        IssueState::Ongoing,
        IssueState::Resolved,
    ] {
        let report = populated_report(state);
        let bytes = report.encode_to_vec();
        let decoded = HealthReport::decode(bytes.as_slice()).expect("decode succeeds");

        let persisted = decoded
            .issues
            .get("docker-permissions")
            .and_then(|issue| issue.persisted_issue.as_ref())
            .expect("persisted_issue is set");
        assert_eq!(persisted.state, state as i32);
        assert_eq!(decoded, report);
    }
}
