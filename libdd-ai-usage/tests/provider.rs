// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The provider helpers: the usage cases in `tests/fixtures/provider-usage`,
//! and the names, error types, ids and DogStatsD packing every integration
//! shares.

use std::fs;
use std::path::{Path, PathBuf};

use libdd_ai_usage::{
    Json, Profile, ProviderAttemptObservation, StreamOutcome, TokenBreakdownObservation,
    UsageFormat, UsageStream, bedrock_arn_account, classify_error, classify_known_error,
    error_type_for_name, error_type_for_status, error_type_identifier, gen_ai_provider_name,
    is_person_identifier, is_service_tier, pack_dogstatsd, parse_provider_body,
    parse_provider_response, project_provider_attempt, project_token_breakdown,
    provider_id_attributes, retain_metrics, with_constant_tags,
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
#[cfg_attr(miri, ignore)] // multi-megabyte lines are too slow under Miri
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
    // A line whose text is longer than the bound is still read: its long
    // strings are cut as they pass, whole in one chunk or split, so the
    // usage read does not depend on the chunk boundaries.
    let long_usage = format!(
        "data: {{\"usage\": {{\"prompt_tokens\": 7, \"completion_tokens\": 1}}, \"pad\": \"{}\"}}\n",
        "x".repeat(libdd_ai_usage::MAX_STREAM_LINE_BYTES)
    );
    let mut whole = UsageStream::new(UsageFormat::OpenAiChat, false);
    whole.feed(long_usage.as_bytes());
    let mut split = UsageStream::new(UsageFormat::OpenAiChat, false);
    let (head, tail) = long_usage.as_bytes().split_at(long_usage.len() / 2);
    split.feed(head);
    split.feed(tail);
    assert_eq!(
        whole.response().usage.as_ref().unwrap().input_tokens,
        Some(7)
    );
    assert_eq!(whole.response(), split.response());
    // A line whose structure alone exceeds the bound is skipped, both ways.
    let huge_structure = format!(
        "data: {{\"usage\": {{\"prompt_tokens\": 7}}, \"pad\": [{}0]}}\n",
        "0,".repeat(libdd_ai_usage::MAX_STREAM_LINE_BYTES / 2 + 1)
    );
    let mut whole = UsageStream::new(UsageFormat::OpenAiChat, false);
    whole.feed(huge_structure.as_bytes());
    let mut split = UsageStream::new(UsageFormat::OpenAiChat, false);
    for piece in huge_structure.as_bytes().chunks(4096) {
        split.feed(piece);
    }
    assert_eq!(whole.response().usage, None);
    assert_eq!(split.response().usage, None);
    // A Responses `response.completed` event carries the whole response on
    // one line, its text included; its usage is read however long the text.
    let completed = format!(
        "event: response.completed\ndata: {{\"type\": \"response.completed\", \"response\": \
         {{\"model\": \"gpt-5\", \"output\": [{{\"type\": \"message\", \"content\": \
         [{{\"type\": \"output_text\", \"text\": \"{}\"}}]}}], \"usage\": \
         {{\"input_tokens\": 11, \"output_tokens\": 4000000}}}}}}\n\n",
        "word \\\"quoted\\\" \\u00e9 ".repeat(300_000)
    );
    assert!(completed.len() > 4 * libdd_ai_usage::MAX_STREAM_LINE_BYTES);
    for step in [completed.len(), 65_536, 1000] {
        let mut stream = UsageStream::new(UsageFormat::OpenAiResponses, false);
        for piece in completed.as_bytes().chunks(step) {
            stream.feed(piece);
        }
        assert_eq!(stream.outcome(), StreamOutcome::Completed, "{step}");
        let usage = stream.response().usage.clone().unwrap();
        assert_eq!(usage.input_tokens, Some(11), "{step}");
        assert_eq!(usage.output_tokens, Some(4_000_000), "{step}");
        assert_eq!(stream.response().response_model.as_deref(), Some("gpt-5"));
    }
    // The same body read whole.
    let body = completed
        .split_once("data: ")
        .unwrap()
        .1
        .trim_end()
        .to_string();
    let (response, outcome, _) =
        parse_provider_body(UsageFormat::OpenAiResponses, body.as_bytes(), false);
    assert_eq!(outcome, StreamOutcome::Completed);
    assert_eq!(response.usage.unwrap().output_tokens, Some(4_000_000));
    // A final line without a line ending is read at the end.
    let mut stream = UsageStream::new(UsageFormat::OpenAiChat, false);
    stream.feed(b"data: [DONE]");
    assert_eq!(stream.finish().1, StreamOutcome::Completed);
}

fn gemini_compat_event(completion: u64, total: u64) -> String {
    format!(
        "data: {{\"choices\": [], \"usage\": {{\"prompt_tokens\": 10, \
         \"completion_tokens\": {completion}, \"total_tokens\": {total}}}}}\n\n"
    )
}

#[test]
#[cfg_attr(miri, ignore)] // multi-megabyte lines are too slow under Miri
fn split_events_read_as_the_stream_does() {
    let events = [
        (
            "message_start",
            r#"{"type": "message_start", "message": {"model": "claude-haiku-4-5",
                "usage": {"input_tokens": 12, "output_tokens": 1}}}"#,
        ),
        (
            "message_delta",
            r#"{"type": "message_delta", "usage": {"output_tokens": 7}}"#,
        ),
        ("message_stop", r#"{"type": "message_stop"}"#),
    ];
    let mut split = UsageStream::new(UsageFormat::Anthropic, false);
    let mut chunked = UsageStream::new(UsageFormat::Anthropic, false);
    for (name, data) in events {
        split.feed_event(name, data);
        let data = data.replace('\n', " ");
        chunked.feed(format!("event: {name}\ndata: {data}\n\n").as_bytes());
    }
    assert_eq!(split.outcome(), StreamOutcome::Completed);
    assert_eq!(split.response(), chunked.response());
    assert_eq!(
        split.response().usage.as_ref().unwrap().output_tokens,
        Some(7)
    );

    // The event name alone ends a stream; data alone is read without one.
    let mut stream = UsageStream::new(UsageFormat::OpenAiResponses, false);
    stream.feed_event("response.completed", "");
    assert_eq!(stream.outcome(), StreamOutcome::Completed);
    let mut stream = UsageStream::new(UsageFormat::OpenAiChat, false);
    stream.feed_event("", "[DONE]");
    assert_eq!(stream.outcome(), StreamOutcome::Completed);
    // Data longer than the line bound is read with its strings cut.
    let mut stream = UsageStream::new(UsageFormat::OpenAiChat, false);
    let long = format!(
        "{{\"usage\": {{\"prompt_tokens\": 3, \"completion_tokens\": 1}}, \"pad\": \"{}\"}}",
        "x".repeat(libdd_ai_usage::MAX_STREAM_LINE_BYTES)
    );
    stream.feed_event("", &long);
    assert_eq!(
        stream.response().usage.as_ref().unwrap().input_tokens,
        Some(3)
    );

    // `[DONE]` ends only an OpenAI-shaped stream.
    for (format, expected) in [
        (UsageFormat::OpenAiChat, StreamOutcome::Completed),
        (UsageFormat::GeminiOpenAiCompat, StreamOutcome::Completed),
        (UsageFormat::Auto, StreamOutcome::Completed),
        (UsageFormat::Anthropic, StreamOutcome::Open),
        (UsageFormat::Gemini, StreamOutcome::Open),
    ] {
        let mut stream = UsageStream::new(format, false);
        stream.feed(b"data: [DONE]\n\n");
        assert_eq!(stream.outcome(), expected, "{format:?}");
    }

    // Anthropic: the output count of `message_start` is a placeholder. A
    // stream cut before `message_delta` reports no output.
    let mut stream = UsageStream::new(UsageFormat::Anthropic, false);
    stream.feed_event("message_start", events[0].1);
    let usage = stream.response().usage.clone().unwrap();
    assert_eq!(usage.input_tokens, Some(12));
    assert_eq!(usage.output_tokens, None);
    assert_eq!(usage.output_basis, None);
}

#[test]
fn stream_basis_follows_the_merged_counts() {
    // Google's OpenAI-compatible endpoint: an early event without thinking
    // reads as output including reasoning; a later one whose total exceeds
    // prompt and completion reports thinking the completion leaves out.
    let mut stream = UsageStream::new(UsageFormat::GeminiOpenAiCompat, false);
    stream.feed(gemini_compat_event(0, 10).as_bytes());
    stream.feed(gemini_compat_event(5, 20).as_bytes());
    let usage = stream.response().usage.clone().unwrap();
    assert_eq!(usage.output_tokens, Some(5));
    assert_eq!(usage.reasoning_output_tokens, Some(5));
    assert_eq!(usage.output_basis, Some("excludes_reasoning"));

    // The basis of the count kept wins also when it came first.
    let mut stream = UsageStream::new(UsageFormat::GeminiOpenAiCompat, false);
    stream.feed(gemini_compat_event(5, 20).as_bytes());
    stream.feed(gemini_compat_event(0, 10).as_bytes());
    let usage = stream.response().usage.clone().unwrap();
    assert_eq!(usage.output_tokens, Some(5));
    assert_eq!(usage.output_basis, Some("excludes_reasoning"));
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
        ("ClientDisconnected", Some("cancelled")),
        ("ReadError", Some("api_connection_error")),
        ("ConnectError", Some("api_connection_error")),
        ("RemoteProtocolError", Some("api_connection_error")),
        ("overloaded_error", Some("internal_server_error")),
        ("invalid_request_error", Some("bad_request_error")),
        // The same failure as HTTP 413.
        ("request_too_large", Some("api_error")),
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
    assert_eq!(
        classify_error(Some("request_too_large"), None),
        classify_error(None, Some(413))
    );
    // A status-shaped name is read as a status only without a real one: the
    // status the response carried wins, and the name is not a fallback.
    assert_eq!(
        classify_error(Some("200"), Some(500)),
        Some("internal_server_error")
    );
    assert_eq!(
        classify_error(Some("404"), Some(500)),
        Some("internal_server_error")
    );
    assert_eq!(classify_error(Some("404"), Some(200)), None);
    assert_eq!(classify_error(Some("404"), None), Some("not_found_error"));
    assert_eq!(classify_error(Some("200"), None), None);
    // Without the fallback, an unknown name is unknown.
    assert_eq!(classify_known_error(Some("SomethingElse"), None), None);
    assert_eq!(
        classify_known_error(Some("SomethingElse"), Some(503)),
        Some("internal_server_error")
    );
    assert_eq!(
        classify_known_error(Some("api_error"), None),
        Some("api_error")
    );
    assert_eq!(
        classify_known_error(Some("RateLimitError"), Some(500)),
        Some("rate_limit_error")
    );
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
    // Without the provider, every known header is tried.
    let openai_only = |name: &str| match name {
        "openai-organization" => Some("acme"),
        "openai-project" => Some("proj_1"),
        _ => None,
    };
    assert_eq!(
        provider_id_attributes("", openai_only),
        provider_id_attributes("openai", openai_only)
    );
    let anthropic_only = |name: &str| (name == "anthropic-organization-id").then_some("org-123");
    assert_eq!(
        provider_id_attributes("", anthropic_only),
        provider_id_attributes("anthropic", anthropic_only)
    );
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
    // A line longer than the payload is dropped, and counted.
    let long = format!("m:1|c|#a:{}", "x".repeat(64));
    let datagrams = pack_dogstatsd(&[line, long.as_str(), line], &[""; 0], 32);
    assert_eq!(datagrams, vec![format!("{line}\n{line}")]);
    let (datagrams, dropped) =
        libdd_ai_usage::pack_dogstatsd_counted(&[line, long.as_str()], &[""; 0], 32);
    assert_eq!((datagrams, dropped), (vec![line.to_string()], 1));
    // The constant tags count toward the length.
    let (datagrams, dropped) = libdd_ai_usage::pack_dogstatsd_counted(&[line], &["env:prod"], 15);
    assert_eq!((datagrams.len(), dropped), (0, 1));
    assert!(pack_dogstatsd(&[""; 0], &[""; 0], 1432).is_empty());
}

#[test]
fn content_chunks_are_skipped_without_losing_what_they_carry() {
    let chunk = |extra: &str| {
        format!(
            "data: {{\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"model\":\"gpt-4o\",\
             \"service_tier\":\"default\",\"choices\":[{{\"index\":0,\"delta\":\
             {{\"content\":\"hi\"}},\"finish_reason\":null}}]{extra}}}\n\n"
        )
    };
    let mut stream = UsageStream::new(UsageFormat::OpenAiChat, false);
    for _ in 0..3 {
        stream.feed(chunk(",\"usage\":null").as_bytes());
    }
    assert_eq!(stream.response().usage, None);
    assert_eq!(stream.response().response_model.as_deref(), Some("gpt-4o"));
    assert_eq!(stream.response().service_tier.as_deref(), Some("default"));
    stream.feed(chunk(",\"usage\" : {\"prompt_tokens\": 5, \"completion_tokens\": 3}").as_bytes());
    // What a report says before the stream ends is what finishing says.
    let report = stream.report();
    stream.feed(b"data: [DONE]\n\n");
    let (response, outcome, error) = stream.finish();
    assert_eq!(response.usage.as_ref().unwrap().input_tokens, Some(5));
    assert_eq!(report.0, response);
    assert_eq!(outcome, StreamOutcome::Completed);
    assert_eq!(error, None);

    // An error named as a value, not a member, is still read.
    let mut stream = UsageStream::new(UsageFormat::OpenAiResponses, false);
    stream.feed(b"data: {\"type\": \"error\", \"code\": \"rate_limit_exceeded\"}\n\n");
    assert_eq!(stream.outcome(), StreamOutcome::Failed);
    assert_eq!(stream.error_name(), Some("rate_limit_exceeded"));

    // A last line without a line ending is read by a report too.
    let mut stream = UsageStream::new(UsageFormat::OpenAiChat, false);
    stream.feed(b"data: {\"usage\": {\"prompt_tokens\": 2, \"completion_tokens\": 1}}");
    assert_eq!(stream.report().0.usage.unwrap().input_tokens, Some(2));
    assert_eq!(stream.response().usage, None);
}
