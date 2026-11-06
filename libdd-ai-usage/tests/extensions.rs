// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The observation extensions: what gateways derive before they state an
//! observation.

use libdd_ai_usage::{
    BillableTokens, ErrorCode, Failure, Json, PricesPerMillion, Profile, error_type_for_failure,
    estimated_cost_nano_usd, expand_observation, operation_name_for, project_extended,
};

fn json(text: &str) -> Json {
    Json::parse(text).unwrap()
}

fn expanded(profile: Profile, text: &str) -> Json {
    expand_observation(profile, &json(text))
        .unwrap()
        .into_owned()
}

#[test]
fn operations_from_routes_and_paths() {
    for (route, operation) in [
        ("llm/v1/chat", Some("chat")),
        ("llm/v1/responses", Some("chat")),
        ("llm/v1/completions", Some("text_completion")),
        ("llm/v1/embeddings", Some("embeddings")),
        ("chat", Some("chat")),
        ("/v1/chat/completions?api-version=2024-10-21", Some("chat")),
        ("/v1/messages", Some("chat")),
        ("/openai/v1/responses/", Some("chat")),
        ("/v1/embeddings", Some("embeddings")),
        ("/v1/completions", Some("text_completion")),
        (
            "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse",
            Some("chat"),
        ),
        (
            "/v1/models/text-embedding-004:embedContent",
            Some("embeddings"),
        ),
        (
            "/model/anthropic.claude-sonnet-4/converse-stream",
            Some("chat"),
        ),
        ("/v1/models", None),
        ("preserve", None),
        ("", None),
    ] {
        assert_eq!(operation_name_for(route), operation, "{route}");
    }
}

#[test]
fn failures_without_a_response() {
    let failure = |status, no_response, timed_out, name| {
        error_type_for_failure(&Failure {
            status,
            no_response,
            timed_out,
            name,
        })
    };
    // Kong: no response, 504 upstream or a timeout is a timeout.
    assert_eq!(failure(Some(504), true, false, None), Some("timeout_error"));
    assert_eq!(failure(None, true, true, None), Some("timeout_error"));
    assert_eq!(
        failure(None, true, false, None),
        Some("api_connection_error")
    );
    assert_eq!(
        failure(Some(502), true, false, None),
        Some("api_connection_error")
    );
    // A response: its status, and a non-200 that is not a failure is api_error.
    assert_eq!(
        failure(Some(429), false, false, None),
        Some("rate_limit_error")
    );
    assert_eq!(failure(Some(204), false, false, None), Some("api_error"));
    assert_eq!(failure(Some(304), false, false, None), Some("api_error"));
    assert_eq!(failure(Some(200), false, false, None), None);
    assert_eq!(failure(None, false, false, None), None);
    // An in-band failure in a 200 is named.
    assert_eq!(
        failure(Some(200), false, false, Some("overloaded_error")),
        Some("overloaded_error").and_then(|n| libdd_ai_usage::classify_error(Some(n), None))
    );
}

#[test]
fn costs_from_prices_per_million() {
    let prices = PricesPerMillion {
        input: 3.0,
        output: 15.0,
        cache_read: Some(0.3),
        cache_write: Some(3.75),
    };
    let tokens = BillableTokens {
        input: 1_000_000,
        cache_read: 400_000,
        cache_write: 100_000,
        output: 10_000,
    };
    // 0.5M at 3 + 0.4M at 0.3 + 0.1M at 3.75 + 0.01M at 15 = 2.145 USD.
    assert_eq!(
        estimated_cost_nano_usd(&prices, &tokens),
        Some(2_145_000_000)
    );
    // Without cache prices every input token is priced as input (Kong).
    let flat = PricesPerMillion {
        cache_read: None,
        cache_write: None,
        ..prices
    };
    assert_eq!(estimated_cost_nano_usd(&flat, &tokens), Some(3_150_000_000));
    let bad = PricesPerMillion {
        input: f64::NAN,
        ..prices
    };
    assert_eq!(estimated_cost_nano_usd(&bad, &tokens), None);
}

const ATTEMPT: &str = r#"{"operation_name": "chat", "provider_name": "anthropic",
    "streaming": false, "observation_point": "gateway", "duration_ns": 1500000000,
    "input_tokens": 100, "input_basis": "excludes_cache", "output_tokens": 20,
    "output_basis": "includes_reasoning", "cache_read_input_tokens": 400,
    "cache_write_input_tokens": 0,
    "prices_per_million": {"input": 3, "output": 15},
    "zero_is_unreported": true}"#;

#[test]
fn an_extended_observation_projects_as_the_one_it_stands_for() {
    let value = expanded(Profile::ProviderAttempt, ATTEMPT);
    assert_eq!(
        value.get("duration_seconds").and_then(Json::as_f64),
        Some(1.5)
    );
    assert!(value.get("duration_ns").is_none());
    // The zero cache write is not reported; the read is.
    assert!(value.get("cache_write_input_tokens").is_none());
    assert_eq!(
        value.get("cache_read_input_tokens").and_then(Json::as_f64),
        Some(400.0)
    );
    // (100 + 400) input at 3 and 20 output at 15, per million.
    assert_eq!(
        value.get("cost_nano_usd").and_then(Json::as_f64),
        Some(1_800_000.0)
    );
    assert_eq!(
        value.get("cost_source").and_then(Json::as_str),
        Some("estimated")
    );
    let projection = project_extended(Profile::ProviderAttempt, &json(ATTEMPT)).unwrap();
    let cost = projection
        .points
        .iter()
        .find(|p| p.name == "trajectory.gen_ai.client.inference.usage.cost")
        .unwrap();
    assert_eq!(cost.exact_value, Some(1_800_000));
    assert_eq!(
        cost.attributes
            .get("trajectory.cost.source")
            .map(String::as_str),
        Some("estimated")
    );

    // A stated cost wins over prices; stated seconds over nanoseconds.
    let stated = ATTEMPT.replace(
        "\"duration_ns\": 1500000000,",
        "\"duration_ns\": 1500000000, \"duration_seconds\": 2.0, \"cost_usd\": 0.5, \"cost_source\": \"reported\",",
    );
    let value = expanded(Profile::ProviderAttempt, &stated);
    assert_eq!(
        value.get("duration_seconds").and_then(Json::as_f64),
        Some(2.0)
    );
    assert!(value.get("cost_nano_usd").is_none());

    // A failure states the error type.
    let failed = ATTEMPT.replace(
        "\"zero_is_unreported\": true",
        "\"failure\": {\"no_response\": true, \"timed_out\": true}",
    );
    let value = expanded(Profile::ProviderAttempt, &failed);
    assert_eq!(
        value.get("error_type").and_then(Json::as_str),
        Some("timeout_error")
    );
}

#[test]
fn zero_totals_are_unreported_only_on_an_estimated_side() {
    let text = r#"{"input_tokens": 0, "input_basis": "includes_cache", "output_tokens": 0,
        "output_basis": "includes_reasoning", "output_token_source": "estimated",
        "reasoning_output_tokens": 0, "zero_is_unreported": true}"#;
    let value = expanded(Profile::TokenBreakdown, text);
    assert_eq!(value.get("input_tokens").and_then(Json::as_f64), Some(0.0));
    assert!(value.get("output_tokens").is_none());
    assert!(value.get("output_basis").is_none());
    assert!(value.get("reasoning_output_tokens").is_none());
    let listed = r#"{"input_tokens": 0, "input_basis": "includes_cache",
        "zero_is_unreported": ["input_tokens"]}"#;
    let value = expanded(Profile::TokenBreakdown, listed);
    assert!(value.get("input_tokens").is_none());
    let error = expand_observation(
        Profile::TokenBreakdown,
        &json(r#"{"zero_is_unreported": ["duration_seconds"]}"#),
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ValueNotAllowed);
}

#[test]
fn nanosecond_durations_and_intervals() {
    let value = expanded(
        Profile::ProviderStreaming,
        r#"{"output_chunk_intervals_ns": [1000000, 250000000]}"#,
    );
    let intervals: Vec<f64> = value
        .get("output_chunk_intervals_seconds")
        .and_then(Json::as_array)
        .unwrap()
        .iter()
        .filter_map(Json::as_f64)
        .collect();
    assert_eq!(intervals, [0.001, 0.25]);
    for bad in [
        r#"{"duration_ns": 1.5}"#,
        r#"{"duration_ns": -1}"#,
        r#"{"duration_ns": "1"}"#,
        r#"{"output_chunk_intervals_ns": 5}"#,
        r#"{"failure": {"status": 42}}"#,
        r#"{"prices_per_million": {"input": 1}}"#,
    ] {
        let error = expand_observation(Profile::ProviderAttempt, &json(bad)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::FieldTypeInvalid, "{bad}");
    }
}

#[test]
fn an_observation_without_extensions_is_not_copied() {
    let plain = json(r#"{"operation_name": "chat"}"#);
    assert!(matches!(
        expand_observation(Profile::ProviderAttempt, &plain).unwrap(),
        std::borrow::Cow::Borrowed(_)
    ));
}

#[test]
fn otel_genai_metrics_translate_to_profile_metrics() {
    use libdd_ai_usage::{Attributes, translate_otel_genai_metric};
    let attrs = |pairs: &[(&str, &str)]| -> Attributes {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    let input = translate_otel_genai_metric(
        "gen_ai.client.token.usage",
        &attrs(&[
            ("gen_ai.token.type", "input"),
            ("gen_ai.system", "aws_bedrock"),
            ("gen_ai.operation.name", "chat"),
        ]),
    )
    .unwrap();
    assert_eq!(input.name, "gen_ai.client.inference.usage.input_tokens");
    assert_eq!(input.profile, Profile::ProviderAttempt);
    assert_eq!(
        input.attributes,
        attrs(&[
            ("gen_ai.operation.name", "chat"),
            ("gen_ai.provider.name", "aws.bedrock"),
        ])
    );
    // Prometheus spelling, as Envoy exposes it.
    let output = translate_otel_genai_metric(
        "gen_ai_client_token_usage_sum",
        &attrs(&[
            ("gen_ai_token_type", "output"),
            ("gen_ai_provider_name", "google-vertex-ai"),
        ]),
    )
    .unwrap();
    assert_eq!(output.name, "gen_ai.client.inference.usage.output_tokens");
    assert_eq!(
        output
            .attributes
            .get("gen_ai.provider.name")
            .map(String::as_str),
        Some("gcp.vertex_ai")
    );
    // A stated provider wins over gen_ai.system.
    let stated = translate_otel_genai_metric(
        "gen_ai.client.token.usage",
        &attrs(&[
            ("gen_ai.token.type", "input"),
            ("gen_ai.system", "openai"),
            ("gen_ai.provider.name", "azure-openai"),
        ]),
    )
    .unwrap();
    assert_eq!(
        stated
            .attributes
            .get("gen_ai.provider.name")
            .map(String::as_str),
        Some("azure.ai.openai")
    );
    // Profile names are kept, in either spelling.
    let breakdown = translate_otel_genai_metric(
        "gen_ai_client_inference_usage_cache_read_input_tokens_total",
        &attrs(&[]),
    )
    .unwrap();
    assert_eq!(
        breakdown.name,
        "gen_ai.client.inference.usage.cache_read.input_tokens"
    );
    assert_eq!(breakdown.profile, Profile::TokenBreakdown);
    for (name, token_type) in [
        ("gen_ai_client_token_usage_count", "input"),
        ("gen_ai_client_token_usage_bucket", "input"),
        ("gen_ai.client.token.usage", "cached"),
        ("http.server.request.duration", "input"),
    ] {
        assert!(
            translate_otel_genai_metric(name, &attrs(&[("gen_ai.token.type", token_type)]))
                .is_none(),
            "{name}"
        );
    }
}

#[test]
fn billing_ids_failures_and_formats() {
    use libdd_ai_usage::{UsageFormat, billing_id, classify_failure};
    assert_eq!(billing_id("  org-123 "), Some("org-123"));
    assert_eq!(billing_id("   "), None);
    assert_eq!(billing_id(&"a".repeat(128)).map(str::len), Some(128));
    assert_eq!(billing_id(&"a".repeat(129)), None);

    assert_eq!(classify_failure(None, None), "api_error");
    assert_eq!(classify_failure(None, Some(200)), "api_error");
    assert_eq!(classify_failure(None, Some(429)), "rate_limit_error");
    assert_eq!(classify_failure(Some("Timeout"), None), "timeout_error");

    for (url, provider, format) in [
        (
            "https://generativelanguage.googleapis.com/v1beta/openai/",
            "openai",
            UsageFormat::GeminiOpenAiCompat,
        ),
        (
            "https://user@GenerativeLanguage.googleapis.com:443/v1beta/openai",
            "",
            UsageFormat::GeminiOpenAiCompat,
        ),
        (
            "https://generativelanguage.googleapis.com/v1beta",
            "gcp.gemini",
            UsageFormat::Auto,
        ),
        (
            "https://proxy.example.com/?next=generativelanguage.googleapis.com",
            "openai",
            UsageFormat::Auto,
        ),
        ("https://api.openai.com/v1", "openai", UsageFormat::Auto),
        ("", "openai", UsageFormat::Auto),
    ] {
        assert_eq!(UsageFormat::for_base_url(url, provider), format, "{url}");
    }
}

#[test]
fn refused_datagrams_are_repacked_and_batches_joined() {
    use libdd_ai_usage::{MetricBatch, pack_dogstatsd_joined, project_result, repack_dogstatsd};
    let line = "m:1|c|#".to_string() + &"t".repeat(23); // 30 bytes
    let datagram = [line.as_str(); 4].join("\n");
    let (text, dropped) = repack_dogstatsd(&datagram, 64, '\0');
    assert_eq!(dropped, 0);
    let parts: Vec<&str> = text.split('\0').collect();
    assert_eq!(parts.len(), 2);
    assert!(
        parts
            .iter()
            .all(|p| p.len() <= 64 && p.lines().count() == 2)
    );
    // A line longer than the socket takes cannot be sent there.
    let long = format!("{datagram}\nm:1|c|#{}", "x".repeat(80));
    let (text, dropped) = repack_dogstatsd(&long, 64, '\0');
    assert_eq!(dropped, 1);
    assert_eq!(text.split('\0').count(), 2);

    let mut batch = MetricBatch::new();
    let observation = json(
        r#"{"operation_name": "chat", "provider_name": "openai", "duration_seconds": 1,
            "streaming": false, "input_tokens": 1, "input_basis": "includes_cache",
            "output_tokens": 1, "output_basis": "includes_reasoning", "observation_point": "gateway"}"#,
    );
    batch
        .add(
            Profile::ProviderAttempt,
            &project_result(Profile::ProviderAttempt, &observation).unwrap(),
        )
        .unwrap();
    let lines = batch.dogstatsd_lines().unwrap();
    assert_eq!(
        batch
            .dogstatsd_datagrams_joined(&["env:prod"], 1432, '\0')
            .unwrap(),
        pack_dogstatsd_joined(lines.iter().map(String::as_str), &["env:prod"], 1432, '\0')
    );
}

#[test]
fn gateway_outcomes_models_inclusive_totals_and_google_errors() {
    use libdd_ai_usage::{
        RESPONSE_MODEL_DROPPED, UsageFormat, error_type_for_gateway_outcome, parse_provider_body,
        parse_provider_response,
    };
    for (outcome, status, expected) in [
        ("timeout", None, "timeout_error"),
        ("unavailable", None, "api_connection_error"),
        ("stream_read_error", None, "api_connection_error"),
        ("early_close", None, "api_connection_error"),
        ("incomplete_stream", None, "api_connection_error"),
        ("canceled", None, "cancelled"),
        ("credentials", None, "api_error"),
        ("unknown", None, "api_error"),
        ("provider_http_error", Some(400), "bad_request_error"),
        ("provider_http_error", Some(401), "authentication_error"),
        ("provider_http_error", Some(403), "permission_denied_error"),
        ("provider_http_error", Some(404), "not_found_error"),
        ("provider_http_error", Some(408), "timeout_error"),
        ("provider_http_error", Some(504), "timeout_error"),
        ("provider_http_error", Some(409), "conflict_error"),
        (
            "provider_http_error",
            Some(422),
            "unprocessable_entity_error",
        ),
        ("provider_http_error", Some(429), "rate_limit_error"),
        ("provider_http_error", Some(502), "internal_server_error"),
        ("provider_http_error", Some(418), "api_error"),
        ("provider_http_error", None, "api_error"),
        ("subscription_auth", Some(401), "authentication_error"),
        (
            "subscription_forbidden",
            Some(403),
            "permission_denied_error",
        ),
        ("subscription_limit", Some(429), "rate_limit_error"),
    ] {
        assert_eq!(
            error_type_for_gateway_outcome(outcome, status),
            expected,
            "{outcome} {status:?}"
        );
    }

    // A served model outside the grammar is not kept, and says so.
    let payload = json(r#"{"model": "free text model", "usage": {"prompt_tokens": 1}}"#);
    let response = parse_provider_response(UsageFormat::OpenAiChat, &payload, false);
    assert_eq!(response.response_model, None);
    assert_eq!(response.issues, vec![RESPONSE_MODEL_DROPPED]);
    let payload = json(r#"{"model": "gpt-4.1-2025-04-14", "usage": {"prompt_tokens": 1}}"#);
    assert!(
        parse_provider_response(UsageFormat::OpenAiChat, &payload, false)
            .issues
            .is_empty()
    );

    // Inclusive totals: Anthropic's cache and a separate reasoning count.
    let payload = json(
        r#"{"usage": {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 100,
            "cache_creation_input_tokens": 20}}"#,
    );
    let usage = parse_provider_response(UsageFormat::Anthropic, &payload, false)
        .usage
        .unwrap()
        .inclusive();
    assert_eq!(
        (usage.input_tokens, usage.input_basis),
        (Some(130), Some("includes_cache"))
    );
    let payload = json(
        r#"{"usage": {"prompt_tokens": 14, "completion_tokens": 1212, "total_tokens": 1459,
            "completion_tokens_details": {"reasoning_tokens": 233}}}"#,
    );
    let usage = parse_provider_response(UsageFormat::OpenAiChat, &payload, false)
        .usage
        .unwrap()
        .inclusive();
    assert_eq!(
        (usage.output_tokens, usage.output_basis),
        (Some(1445), Some("includes_reasoning"))
    );

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
        assert_eq!(outcome, libdd_ai_usage::StreamOutcome::Failed);
        assert_eq!(
            libdd_ai_usage::classify_error(name.as_deref(), None),
            Some(expected),
            "{status}"
        );
    }

    // OpenAI image output tokens are their own modality part, within the
    // output total.
    let payload = json(
        r#"{"usage": {"input_tokens": 50, "output_tokens": 4160,
            "output_tokens_details": {"image_tokens": 4160}}}"#,
    );
    let usage = parse_provider_response(UsageFormat::OpenAiResponses, &payload, false)
        .usage
        .unwrap();
    assert_eq!(usage.output_tokens, Some(4160));
    assert_eq!(
        usage
            .output_tokens_by_modality
            .unwrap()
            .get("image")
            .copied(),
        Some(4160)
    );
}
