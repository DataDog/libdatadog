// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The provider helpers: the usage cases in `tests/fixtures/provider-usage`,
//! and the names, error types, ids and DogStatsD packing every integration
//! shares.

use std::fs;
use std::path::{Path, PathBuf};

use libdd_ai_usage::{
    Json, Profile, ProviderAttemptObservation, StreamOutcome, TokenBreakdownObservation,
    UsageFormat, UsageStream, bedrock_arn_account, classify_error, error_type_for_name,
    error_type_for_status, error_type_identifier, gen_ai_provider_name, is_person_identifier,
    is_service_tier, pack_dogstatsd, parse_provider_body, parse_provider_response,
    project_provider_attempt, project_token_breakdown, provider_id_attributes, retain_metrics,
    with_constant_tags,
};
use serde_json::{Value, json};

fn cases() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/provider-usage");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    files
}

/// `actual`'s members, each compared with `expected`'s, where an absent
/// expected member is `null`.
fn mismatches(actual: &Value, expected: &Value) -> Vec<String> {
    actual
        .as_object()
        .unwrap()
        .iter()
        .filter(|(key, value)| expected.get(key.as_str()).unwrap_or(&Value::Null) != *value)
        .map(|(key, value)| {
            format!(
                "{key}: got {value}, expected {}",
                expected.get(key.as_str()).unwrap_or(&Value::Null)
            )
        })
        .collect()
}

#[test]
fn provider_usage_cases() {
    let files = cases();
    assert!(files.len() >= 30, "found {} cases", files.len());
    let mut failures = Vec::new();
    for path in files {
        let case: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let format = UsageFormat::from_name(case["format"].as_str().unwrap()).unwrap();
        let embeddings = case["operation_name"] == "embeddings";
        let expected = &case["expected"];
        let actual = if let Some(body) = case["body"].as_str() {
            let (response, outcome, error) =
                parse_provider_body(format, body.as_bytes(), embeddings);
            let mut value = serde_json::to_value(&response).unwrap();
            value["outcome"] = json!(outcome.as_str());
            value["error_name"] = json!(error);
            // The same stream read one byte at a time, which splits every
            // line and every CRLF.
            if !body.trim_start().starts_with(['{', '[']) {
                let mut stream = UsageStream::new(format, embeddings);
                for byte in body.as_bytes() {
                    stream.feed(std::slice::from_ref(byte));
                }
                let (bytewise, bytewise_outcome, bytewise_error) = stream.finish();
                if (bytewise, bytewise_outcome, bytewise_error) != (response, outcome, error) {
                    failures.push(format!("{name}: reading byte by byte differs"));
                }
            }
            value
        } else {
            let payload = Json::parse(&case["payload"].to_string()).unwrap();
            serde_json::to_value(parse_provider_response(format, &payload, embeddings)).unwrap()
        };
        for mismatch in mismatches(&actual, expected) {
            failures.push(format!("{name}: {mismatch}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn parsed_usage_projects() {
    // An Anthropic response as the observation of one attempt: the crate
    // adds the cache back into the input total.
    let payload = Json::parse(
        r#"{"model": "claude-sonnet-4-5", "usage": {"input_tokens": 20, "output_tokens": 5,
            "cache_read_input_tokens": 100, "cache_creation_input_tokens": 30,
            "cache_creation": {"ephemeral_5m_input_tokens": 30, "ephemeral_1h_input_tokens": 0}}}"#,
    )
    .unwrap();
    let response = parse_provider_response(UsageFormat::Anthropic, &payload, false);
    let usage = response.usage.unwrap();
    let mut attempt = ProviderAttemptObservation {
        operation_name: Some("chat".into()),
        provider_name: Some("anthropic".into()),
        duration_seconds: Some(1.0),
        streaming: Some(false),
        observation_point: Some("gateway".into()),
        ..ProviderAttemptObservation::default()
    };
    usage.apply_to_attempt(&mut attempt);
    let projection = project_provider_attempt(&attempt).unwrap();
    let input = projection
        .points
        .iter()
        .find(|p| p.name == "gen_ai.client.inference.usage.input_tokens")
        .unwrap();
    assert_eq!(input.value, 150.0);
    let mut breakdown = TokenBreakdownObservation {
        operation_name: Some("chat".into()),
        provider_name: Some("anthropic".into()),
        observation_point: Some("gateway".into()),
        ..TokenBreakdownObservation::default()
    };
    usage.apply_to_breakdown(&mut breakdown);
    let breakdown = project_token_breakdown(&breakdown).unwrap();
    assert!(breakdown.issues.is_empty(), "{:?}", breakdown.issues);
    assert!(breakdown.points.iter().any(|p| p.name
        == "gen_ai.client.inference.usage.cache_read.input_tokens"
        && p.value == 100.0));

    let mut projection = projection;
    retain_metrics(&mut projection, &["gen_ai.client.inference.duration"]);
    assert!(
        projection
            .points
            .iter()
            .all(|p| p.name == "gen_ai.client.inference.duration")
    );
    assert!(!projection.points.is_empty());
    assert!(
        Profile::ProviderAttempt
            .metric_names()
            .contains(&"gen_ai.client.inference.duration")
    );
}

#[test]
fn stream_outcomes() {
    let mut stream = UsageStream::new(UsageFormat::Auto, false);
    assert_eq!(stream.outcome(), StreamOutcome::Open);
    stream.feed(b"event: message_stop\n");
    assert_eq!(stream.outcome(), StreamOutcome::Completed);

    // A line longer than the bound is skipped, and the next line is read.
    let mut stream = UsageStream::new(UsageFormat::OpenAiChat, false);
    stream.feed(b"data: {\"usage\": \"");
    stream.feed(&vec![b'x'; libdd_ai_usage::MAX_STREAM_LINE_BYTES]);
    stream.feed(b"\"}\ndata: {\"usage\": {\"prompt_tokens\": 3, \"completion_tokens\": 1}}\n");
    assert_eq!(
        stream.response().usage.as_ref().unwrap().input_tokens,
        Some(3)
    );
    // A final line without a line ending is read at the end.
    let mut stream = UsageStream::new(UsageFormat::OpenAiChat, false);
    stream.feed(b"data: [DONE]");
    assert_eq!(stream.finish().1, StreamOutcome::Completed);
}

#[test]
fn provider_names() {
    for (name, expected) in [
        ("bedrock", Some("aws.bedrock")),
        ("bedrock_converse", Some("aws.bedrock")),
        ("azure", Some("azure.ai.openai")),
        ("azure_ai", Some("azure.ai.inference")),
        ("vertex_ai", Some("gcp.vertex_ai")),
        ("gemini", Some("gcp.gemini")),
        ("xai", Some("x_ai")),
        ("mistral", Some("mistral_ai")),
        ("watsonx", Some("ibm.watsonx.ai")),
        ("text-completion-openai", Some("openai")),
        ("anthropic", Some("anthropic")),
        ("aws.bedrock", Some("aws.bedrock")),
        (" OpenAI ", Some("openai")),
        ("Hugging Face", Some("hugging_face")),
        ("my/provider", Some("my_provider")),
        ("1cloud", None),
        ("", None),
    ] {
        assert_eq!(gen_ai_provider_name(name).as_deref(), expected, "{name}");
    }
}

#[test]
fn error_types() {
    for (status, expected) in [
        (200, None),
        (400, Some("bad_request_error")),
        (401, Some("authentication_error")),
        (403, Some("permission_denied_error")),
        (404, Some("not_found_error")),
        (408, Some("timeout_error")),
        (409, Some("conflict_error")),
        (413, Some("api_error")),
        (422, Some("unprocessable_entity_error")),
        (429, Some("rate_limit_error")),
        (500, Some("internal_server_error")),
        (502, Some("internal_server_error")),
        (504, Some("timeout_error")),
        (529, Some("internal_server_error")),
    ] {
        assert_eq!(error_type_for_status(status), expected, "{status}");
    }
    for (name, expected) in [
        ("RateLimitError", Some("rate_limit_error")),
        ("APITimeoutError", Some("timeout_error")),
        ("Timeout", Some("timeout_error")),
        ("APIConnectionError", Some("api_connection_error")),
        ("CancelledError", Some("cancelled")),
        ("overloaded_error", Some("internal_server_error")),
        ("invalid_request_error", Some("bad_request_error")),
        ("rate_limit_error", Some("rate_limit_error")),
        ("SomethingElse", None),
    ] {
        assert_eq!(error_type_for_name(name), expected, "{name}");
    }
    for (name, expected) in [
        ("RateLimitError", Some("rate_limit_error")),
        ("APIConnectionError", Some("api_connection_error")),
        (
            "ContextWindowExceededError",
            Some("context_window_exceeded_error"),
        ),
        ("HTTPError", Some("http_error")),
        ("Timeout", Some("timeout")),
        ("_Private", None),
    ] {
        assert_eq!(error_type_identifier(name).as_deref(), expected, "{name}");
    }
    assert_eq!(classify_error(None, None), None);
    assert_eq!(classify_error(None, Some(200)), None);
    assert_eq!(classify_error(None, Some(429)), Some("rate_limit_error"));
    assert_eq!(classify_error(Some("429"), None), Some("rate_limit_error"));
    // A known name wins over the status a transport translated it into.
    assert_eq!(
        classify_error(Some("APITimeoutError"), Some(500)),
        Some("timeout_error")
    );
    assert_eq!(
        classify_error(Some("SomethingElse"), Some(503)),
        Some("internal_server_error")
    );
    assert_eq!(
        classify_error(Some("SomethingElse"), None),
        Some("api_error")
    );
}

#[test]
fn provider_ids() {
    let headers = [
        ("anthropic-organization-id", "org-123"),
        ("openai-organization", "acme"),
        ("openai-project", " proj_1 "),
    ];
    let header = |name: &str| {
        headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| *value)
    };
    let anthropic = provider_id_attributes("anthropic", header);
    assert_eq!(
        anthropic
            .get("trajectory.request.provider_account_id")
            .map(String::as_str),
        Some("org-123")
    );
    assert_eq!(anthropic.len(), 1);
    let openai = provider_id_attributes("openai", header);
    assert_eq!(
        openai
            .get("trajectory.request.project_id")
            .map(String::as_str),
        Some("proj_1")
    );
    assert_eq!(
        openai
            .get("trajectory.request.provider_account_id")
            .map(String::as_str),
        Some("acme")
    );
    assert!(provider_id_attributes("aws.bedrock", header).is_empty());
    let long = "x".repeat(129);
    assert!(provider_id_attributes("anthropic", |_| Some(long.as_str())).is_empty());

    for (model, expected) in [
        (
            "arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude",
            Some("123456789012"),
        ),
        (
            "bedrock/arn:aws-us-gov:bedrock:us-gov-west-1:210987654321:application-inference-profile/x",
            Some("210987654321"),
        ),
        ("xarn:aws:bedrock:us-east-1:123456789012:foo", None),
        ("arn:aws:bedrock:us-east-1:12345:foo", None),
        ("arn:aws:s3:us-east-1:123456789012:foo", None),
        ("arn:aws:bedrock:us-east-1:123456789012", None),
        ("anthropic.claude-3-haiku", None),
    ] {
        assert_eq!(bedrock_arn_account(model), expected, "{model}");
    }
}

#[test]
fn identity_and_tiers() {
    assert!(is_person_identifier("ada@example.com"));
    assert!(is_person_identifier("a.b+c@sub.example.co"));
    for value in [
        "service-account",
        "@example.com",
        "ada@",
        "ada@example",
        "ada@.com",
        "ada@example.",
        "a b@example.com",
        "ada@ex@ample.com",
    ] {
        assert!(!is_person_identifier(value), "{value}");
    }
    assert!(is_service_tier("default"));
    assert!(is_service_tier("ON_DEMAND"));
    assert!(!is_service_tier(""));
    assert!(!is_service_tier("has space"));
    assert!(!is_service_tier(&"x".repeat(33)));
}

#[test]
fn dogstatsd_packing() {
    assert_eq!(
        with_constant_tags("m:1|c|#a:b", &["env:prod", "version:1"]),
        "m:1|c|#a:b,env:prod,version:1"
    );
    assert_eq!(
        with_constant_tags("m:1|c", &["env:prod"]),
        "m:1|c|#env:prod"
    );
    assert_eq!(with_constant_tags("m:1|c", &[""; 0]), "m:1|c");

    let line = "m:1|c|#a:b"; // 10 bytes, 21 with the constant tag
    let lines = vec![line; 5];
    let datagrams = pack_dogstatsd(&lines, &["env:prod"], 43);
    // Two lines and their newline make 43 bytes.
    assert_eq!(datagrams.len(), 3);
    assert!(datagrams.iter().all(|d| d.len() <= 43));
    assert_eq!(datagrams[0], "m:1|c|#a:b,env:prod\nm:1|c|#a:b,env:prod");
    assert_eq!(
        datagrams.iter().map(|d| d.lines().count()).sum::<usize>(),
        5
    );
    // A line longer than the payload is a datagram of its own.
    let long = format!("m:1|c|#a:{}", "x".repeat(64));
    let datagrams = pack_dogstatsd(&[line, long.as_str(), line], &[""; 0], 32);
    assert_eq!(datagrams, vec![line, long.as_str(), line]);
    assert!(pack_dogstatsd(&[""; 0], &[""; 0], 1432).is_empty());
}
