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
fn reasoning_basis_follows_the_total_so_output_is_billed_once() {
    // Vertex AI's OpenAI-compatible API (completion leaves reasoning out)
    // and OpenAI's own shape (completion includes it) both bill 1445.
    for (completion, label) in [(1212, "separate"), (1445, "included")] {
        let payload = Json::parse(&format!(
            r#"{{"usage": {{"prompt_tokens": 14, "completion_tokens": {completion},
                "total_tokens": 1459, "completion_tokens_details": {{"reasoning_tokens": 233}}}}}}"#
        ))
        .unwrap();
        for format in [UsageFormat::OpenAiChat, UsageFormat::Auto] {
            let usage = parse_provider_response(format, &payload, false)
                .usage
                .unwrap();
            let mut attempt = ProviderAttemptObservation {
                operation_name: Some("chat".into()),
                provider_name: Some("gcp.vertex_ai".into()),
                duration_seconds: Some(1.0),
                streaming: Some(false),
                observation_point: Some("gateway".into()),
                ..ProviderAttemptObservation::default()
            };
            usage.apply_to_attempt(&mut attempt);
            let projection = project_provider_attempt(&attempt).unwrap();
            let output = projection
                .points
                .iter()
                .find(|p| p.name == "gen_ai.client.inference.usage.output_tokens")
                .unwrap();
            assert_eq!(output.value, 1445.0, "{label} {format:?}");
        }
    }
    // Totals that match neither keep OpenAI's reading.
    let payload = Json::parse(
        r#"{"usage": {"prompt_tokens": 14, "completion_tokens": 1212, "total_tokens": 2000,
            "completion_tokens_details": {"reasoning_tokens": 233}}}"#,
    )
    .unwrap();
    let usage = parse_provider_response(UsageFormat::OpenAiChat, &payload, false)
        .usage
        .unwrap();
    assert_eq!(usage.output_basis, Some("includes_reasoning"));
}

#[test]
fn a_dropped_service_tier_is_reported_until_a_tier_is_kept() {
    let mut stream = UsageStream::new(UsageFormat::OpenAiChat, false);
    stream.feed(b"data: {\"service_tier\": \"flex tier\", \"choices\": []}\n\n");
    assert_eq!(
        stream.report().0.issues,
        vec![libdd_ai_usage::SERVICE_TIER_DROPPED]
    );
    stream.feed(b"data: {\"service_tier\": \"flex\", \"choices\": []}\n\n");
    let (response, ..) = stream.finish();
    assert_eq!(response.service_tier.as_deref(), Some("flex"));
    assert!(response.issues.is_empty());
}

#[test]
fn provider_errors_carry_a_safe_message_and_code() {
    // The Responses error event holds its code and message beside its type.
    let mut stream = UsageStream::new(UsageFormat::OpenAiResponses, false);
    stream.feed(
        b"event: error\ndata: {\"type\": \"error\", \"code\": \"rate_limit_exceeded\", \"message\": \"Too\\nmany\\u0007 requests\"}\n\n",
    );
    let (response, outcome, name) = stream.finish();
    assert_eq!(outcome, StreamOutcome::Failed);
    assert_eq!(name.as_deref(), Some("rate_limit_exceeded"));
    let error = response.error.unwrap();
    assert_eq!(error.name.as_deref(), Some("rate_limit_exceeded"));
    assert_eq!(error.code.as_deref(), Some("rate_limit_exceeded"));
    // Control characters never reach a log line.
    assert_eq!(error.message.as_deref(), Some("Too many  requests"));

    // A long message is cut at a character boundary.
    let long = "é".repeat(400);
    let payload = Json::parse(&format!(
        r#"{{"error": {{"type": "invalid_request_error", "code": 400, "message": "{long}"}}}}"#
    ))
    .unwrap();
    let error = parse_provider_response(UsageFormat::OpenAiChat, &payload, false)
        .error
        .unwrap();
    assert_eq!(error.name.as_deref(), Some("invalid_request_error"));
    assert_eq!(error.code.as_deref(), Some("400"));
    let message = error.message.unwrap();
    assert!(message.len() <= libdd_ai_usage::MAX_ERROR_MESSAGE_BYTES);
    assert!(message.len() > libdd_ai_usage::MAX_ERROR_MESSAGE_BYTES - 2);
    assert!(message.chars().all(|c| c == 'é'));

    // A bare message, and no error at all.
    let payload = Json::parse(r#"{"error": "upstream unavailable"}"#).unwrap();
    let error = parse_provider_response(UsageFormat::Auto, &payload, false)
        .error
        .unwrap();
    assert_eq!(error.message.as_deref(), Some("upstream unavailable"));
    assert_eq!(error.name, None);
    let payload = Json::parse(r#"{"error": null, "usage": {"prompt_tokens": 1}}"#).unwrap();
    assert_eq!(
        parse_provider_response(UsageFormat::Auto, &payload, false).error,
        None
    );
}

/// What the token-breakdown profile records from one provider response:
/// the uncached input, whether a cache-write point was recorded, and the
/// issues.
fn breakdown_of(
    format: UsageFormat,
    provider: &str,
    payload: &str,
) -> (Option<f64>, bool, Vec<&'static str>) {
    let payload = Json::parse(payload).unwrap();
    let usage = parse_provider_response(format, &payload, false)
        .usage
        .unwrap();
    let mut observation = TokenBreakdownObservation {
        operation_name: Some("chat".into()),
        provider_name: gen_ai_provider_name(provider),
        observation_point: Some("gateway".into()),
        ..TokenBreakdownObservation::default()
    };
    usage.apply_to_breakdown(&mut observation);
    let projection = project_token_breakdown(&observation).unwrap();
    let uncached = projection
        .points
        .iter()
        .find(|p| p.name == "trajectory.gen_ai.client.inference.usage.uncached.input_tokens")
        .map(|p| p.value);
    let wrote = projection
        .points
        .iter()
        .any(|p| p.name == "gen_ai.client.inference.usage.cache_write.input_tokens");
    let issues = projection
        .issues
        .iter()
        .map(|issue| issue.as_str())
        .collect();
    (uncached, wrote, issues)
}

#[test]
fn uncached_input_of_providers_without_cache_writes() {
    let chat = r#"{"usage": {"prompt_tokens": 1200, "completion_tokens": 300,
        "prompt_tokens_details": {"cached_tokens": 1024}}}"#;
    let chat_uncached = r#"{"usage": {"prompt_tokens": 12, "completion_tokens": 2}}"#;
    let responses = r#"{"usage": {"input_tokens": 33, "output_tokens": 5,
        "input_tokens_details": {"cached_tokens": 20},
        "output_tokens_details": {"reasoning_tokens": 2}}}"#;
    let responses_uncached = r#"{"usage": {"input_tokens": 33, "output_tokens": 5}}"#;
    let responses_written = r#"{"usage": {"input_tokens": 5000, "output_tokens": 700,
        "input_tokens_details": {"cached_tokens": 4096, "cache_write_tokens": 512}}}"#;
    let gemini = r#"{"usageMetadata": {"promptTokenCount": 4000, "candidatesTokenCount": 100,
        "cachedContentTokenCount": 3000, "totalTokenCount": 4100}}"#;
    let normalized_anthropic = r#"{"usage": {"prompt_tokens": 1700, "completion_tokens": 80,
        "prompt_tokens_details": {"cached_tokens": 1000}}}"#;
    use UsageFormat::{Gemini, OpenAiChat, OpenAiResponses};
    let cases = [
        // OpenAI reports cached tokens and never a cache write: the cache
        // write is zero, and no cache-write point is recorded.
        (
            "openai chat",
            OpenAiChat,
            "openai",
            chat,
            Some(176.0),
            false,
            vec![],
        ),
        (
            "openai responses",
            OpenAiResponses,
            "openai",
            responses,
            Some(13.0),
            false,
            vec![],
        ),
        (
            "azure chat",
            OpenAiChat,
            "azure",
            chat,
            Some(176.0),
            false,
            vec![],
        ),
        (
            "gemini",
            Gemini,
            "gemini",
            gemini,
            Some(1000.0),
            false,
            vec![],
        ),
        // Without cached tokens the cache reads, and so the uncached input,
        // are not known.
        (
            "openai chat, no cached tokens",
            OpenAiChat,
            "openai",
            chat_uncached,
            None,
            false,
            vec!["cache_read_missing"],
        ),
        (
            "openai responses, no cached tokens",
            OpenAiResponses,
            "openai",
            responses_uncached,
            None,
            false,
            vec!["cache_read_missing"],
        ),
        // A reported cache write is used as reported.
        (
            "openai responses, cache write",
            OpenAiResponses,
            "openai",
            responses_written,
            Some(392.0),
            true,
            vec!["cache_write_lifetime_unknown"],
        ),
        // A provider with cache writes that reported none: not known.
        (
            "anthropic in the chat shape",
            OpenAiChat,
            "anthropic",
            normalized_anthropic,
            None,
            false,
            vec!["cache_write_missing"],
        ),
        (
            "bedrock in the chat shape",
            OpenAiChat,
            "bedrock",
            normalized_anthropic,
            None,
            false,
            vec!["cache_write_missing"],
        ),
        // Vertex AI also serves models with cache writes.
        (
            "gemini on vertex",
            Gemini,
            "vertex_ai",
            gemini,
            None,
            false,
            vec!["cache_write_missing"],
        ),
    ];
    for (name, format, provider, payload, uncached, wrote, issues) in cases {
        assert_eq!(
            breakdown_of(format, provider, payload),
            (uncached, wrote, issues),
            "{name}"
        );
    }
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
    // A line longer than the payload but not than the longest line sent
    // goes alone in a datagram of its own, and is never joined.
    let long = format!("m:1|c|#a:{}", "x".repeat(64));
    let datagrams = pack_dogstatsd(&[line, long.as_str(), line, line], &[""; 0], 32);
    assert_eq!(
        datagrams,
        vec![line.to_string(), long.clone(), format!("{line}\n{line}")]
    );
    // Only a line longer than 8192 bytes is dropped, and counted; the
    // constant tags count toward its length.
    let edge = format!("m:1|c|#a:{}", "x".repeat(8192 - 9));
    assert_eq!(edge.len(), 8192);
    let (datagrams, dropped) =
        libdd_ai_usage::pack_dogstatsd_counted(&[line, edge.as_str(), line], &[""; 0], 32);
    assert_eq!((datagrams.len(), dropped), (3, 0));
    let (datagrams, dropped) =
        libdd_ai_usage::pack_dogstatsd_counted(&[line, edge.as_str(), line], &["env:prod"], 32);
    let tagged = format!("{line},env:prod");
    assert_eq!((datagrams, dropped), (vec![tagged.clone(), tagged], 1));
    assert!(pack_dogstatsd(&[""; 0], &[""; 0], 1432).is_empty());

    // Over UDP, a line with many tags passes 1432 bytes and is still sent,
    // alone; one past 8192 bytes is dropped.
    let tags: Vec<String> = (0..120)
        .map(|i| format!("tag_{i:03}:value_{i:03}"))
        .collect();
    let wide = format!("m:1|c|#{}", tags.join(","));
    assert!(wide.len() > 1432 && wide.len() <= libdd_ai_usage::DOGSTATSD_MAX_LINE_BYTES);
    let huge = format!("m:1|c|#a:{}", "x".repeat(9000));
    let lines = [line, wide.as_str(), huge.as_str(), line];
    let (datagrams, dropped) = libdd_ai_usage::pack_dogstatsd_counted(&lines, &["env:prod"], 1432);
    assert_eq!(dropped, 1);
    assert_eq!(datagrams.len(), 3);
    assert_eq!(datagrams[1], format!("{wide},env:prod"));
    let (joined, joined_dropped) =
        libdd_ai_usage::pack_dogstatsd_joined(lines, &["env:prod"], 1432, '\0');
    assert_eq!((joined, joined_dropped), (datagrams.join("\0"), 1));
    // A payload larger than the default longest line is the longest line.
    let (datagrams, dropped) =
        libdd_ai_usage::pack_dogstatsd_counted(&[huge.as_str()], &[""; 0], 16384);
    assert_eq!((datagrams.len(), dropped), (1, 0));

    // Packed into one text, the datagrams are the same.
    let lines = [
        "a:1|c|#x:y",
        "b:2|c",
        "",
        "c:3|d|#k:v,w:z",
        long.as_str(),
        "d:4|c",
        "e:5|c|#t:u",
    ];
    for tags in [
        &[][..],
        &["env:prod"][..],
        &["env:prod", "", "version:1"][..],
    ] {
        for max in [10, 16, 25, 32, 43, 64, 1432] {
            let (datagrams, dropped) = libdd_ai_usage::pack_dogstatsd_counted(&lines, tags, max);
            let (joined, joined_dropped) =
                libdd_ai_usage::pack_dogstatsd_joined(lines, tags, max, '\0');
            assert_eq!(joined, datagrams.join("\0"), "{tags:?} {max}");
            assert_eq!(joined_dropped, dropped, "{tags:?} {max}");
        }
    }
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

#[test]
fn served_models_google_errors_names_and_vertex_projects() {
    use libdd_ai_usage::{RESPONSE_MODEL_DROPPED, error_type_for_name, vertex_project_id};
    // A served model outside the grammar is not kept, and says so.
    let payload =
        Json::parse(r#"{"model": "free text model", "usage": {"prompt_tokens": 1}}"#).unwrap();
    let response = parse_provider_response(UsageFormat::OpenAiChat, &payload, false);
    assert_eq!(response.response_model, None);
    assert_eq!(response.issues, vec![RESPONSE_MODEL_DROPPED]);

    // Google's array-wrapped error envelope, typed by its `status`.
    for (status, expected) in [
        ("RESOURCE_EXHAUSTED", "rate_limit_error"),
        ("CANCELLED", "cancelled"),
        ("DEADLINE_EXCEEDED", "timeout_error"),
        ("UNAUTHENTICATED", "authentication_error"),
    ] {
        let body =
            format!(r#"[{{"error": {{"code": 499, "message": "m", "status": "{status}"}}}}]"#);
        let (_, outcome, name) = parse_provider_body(UsageFormat::Gemini, body.as_bytes(), false);
        assert_eq!(outcome, StreamOutcome::Failed);
        assert_eq!(
            classify_error(name.as_deref(), None),
            Some(expected),
            "{status}"
        );
    }

    // Vertex AI's served tier.
    let payload = Json::parse(
        r#"{"usageMetadata": {"promptTokenCount": 3, "candidatesTokenCount": 1,
            "trafficType": "ON_DEMAND"}}"#,
    )
    .unwrap();
    assert_eq!(
        parse_provider_response(UsageFormat::Gemini, &payload, false)
            .service_tier
            .as_deref(),
        Some("ON_DEMAND")
    );

    // OpenAI image output tokens are their own modality part, within the
    // output total.
    let payload = Json::parse(
        r#"{"usage": {"input_tokens": 50, "output_tokens": 4160,
            "output_tokens_details": {"image_tokens": 4160}}}"#,
    )
    .unwrap();
    let usage = parse_provider_response(UsageFormat::OpenAiResponses, &payload, false)
        .usage
        .unwrap();
    assert_eq!(
        usage
            .output_tokens_by_modality
            .unwrap()
            .get("image")
            .copied(),
        Some(4160)
    );

    // Error names in any case.
    assert_eq!(
        error_type_for_name("RATE_LIMIT_ERROR"),
        Some("rate_limit_error")
    );
    assert_eq!(
        error_type_for_name("ratelimiterror"),
        Some("rate_limit_error")
    );
    assert_eq!(
        error_type_for_name("Overloaded_Error"),
        Some("internal_server_error")
    );
    assert_eq!(error_type_for_name("timeout"), Some("timeout_error"));

    // Google's status names match only as Google spells them. In another case
    // they are ordinary words, which name no failure of their own.
    for (google, plain, expected) in [
        ("UNKNOWN", "unknown", "internal_server_error"),
        ("INTERNAL", "internal", "internal_server_error"),
        ("NOT_FOUND", "not_found", "not_found_error"),
        ("UNAVAILABLE", "unavailable", "internal_server_error"),
        ("ABORTED", "aborted", "conflict_error"),
        (
            "PERMISSION_DENIED",
            "permission_denied",
            "permission_denied_error",
        ),
        (
            "RESOURCE_EXHAUSTED",
            "resource_exhausted",
            "rate_limit_error",
        ),
        ("DEADLINE_EXCEEDED", "Deadline_Exceeded", "timeout_error"),
    ] {
        assert_eq!(error_type_for_name(google), Some(expected), "{google}");
        assert_eq!(error_type_for_name(plain), None, "{plain}");
        assert_eq!(classify_known_error(Some(plain), None), None, "{plain}");
        // A plain name is a failure whose cause is unknown, or its status.
        assert_eq!(
            classify_error(Some(plain), None),
            Some("api_error"),
            "{plain}"
        );
        assert_eq!(
            classify_error(Some(plain), Some(404)),
            Some("not_found_error"),
            "{plain}"
        );
    }
    // `cancelled` is a value of the vocabulary itself, in any case; Google's
    // `CANCELLED` means the same.
    for name in ["cancelled", "Cancelled", "CANCELLED"] {
        assert_eq!(error_type_for_name(name), Some("cancelled"), "{name}");
    }

    // The Vertex AI project an upstream URL bills.
    assert_eq!(
        vertex_project_id(
            "https://us-east5-aiplatform.googleapis.com/v1/projects/my-proj/locations/us-east5/publishers/anthropic/models/claude:rawPredict"
        ),
        Some("my-proj")
    );
    assert_eq!(
        vertex_project_id("https://api.openai.com/v1/projects/x"),
        None
    );
    assert_eq!(
        vertex_project_id("https://aiplatform.googleapis.com/v1/models"),
        None
    );
}
