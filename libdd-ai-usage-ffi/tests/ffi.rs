// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The C interface as a caller sees it: slices and JSON in, string wrappers
//! out, error pointers, and handles. The projection and parsing rules are
//! libdd-ai-usage's and are tested there.

use std::ffi::CStr;
use std::ptr;

use dd_ai_usage_ffi::*;
use libdd_common_ffi::slice::ByteSlice;
use libdd_common_ffi::{CharSlice, Handle, StringWrapper, ddog_StringWrapper_drop};
use serde_json::{Value, json};

type Failure = Option<(AiUsageErrorCode, String)>;

fn cs(text: &str) -> CharSlice<'_> {
    CharSlice::from(text)
}

fn bs(bytes: &[u8]) -> ByteSlice<'_> {
    ByteSlice::from(bytes)
}

/// The code and message of an error, released; `None` for success.
fn failure(error: Option<Box<AiUsageError>>) -> Failure {
    error.map(|error| {
        // SAFETY: a static NUL-terminated string.
        let name = unsafe { CStr::from_ptr(ddog_ai_usage_error_code_name(Some(&error))) };
        assert!(!name.to_bytes().is_empty());
        let result = (error.code, error.message.to_string());
        ddog_ai_usage_error_free(Some(error));
        result
    })
}

fn code(error: Option<Box<AiUsageError>>) -> Option<AiUsageErrorCode> {
    failure(error).map(|(code, _)| code)
}

/// Run a call with an output, and return its failure and output text.
fn call(f: impl FnOnce(*mut StringWrapper) -> Option<Box<AiUsageError>>) -> (Failure, String) {
    let mut out = StringWrapper::from(String::new());
    let error = failure(f(&mut out));
    (error, String::from(out))
}

fn ok_json(f: impl FnOnce(*mut StringWrapper) -> Option<Box<AiUsageError>>) -> Value {
    let (error, text) = call(f);
    assert_eq!(error, None, "{text}");
    serde_json::from_str(&text).unwrap()
}

fn new_batch() -> Handle<Batch> {
    let mut batch = Handle::empty();
    // SAFETY: `batch` is writable.
    assert_eq!(code(unsafe { ddog_ai_usage_batch_new(&mut batch) }), None);
    batch
}

fn new_stream(format: &str, options: u32) -> Handle<Stream> {
    let mut stream = Handle::empty();
    // SAFETY: a valid slice and a writable handle.
    let error = unsafe { ddog_ai_usage_stream_new_with(cs(format), false, options, &mut stream) };
    assert_eq!(code(error), None);
    stream
}

const ATTEMPT: &str = r#"{"profile": "gen_ai.client.provider_attempt@0.1.0",
    "observation": {"operation_name": "chat", "provider_name": "openai",
        "duration_seconds": 0.5, "streaming": false, "input_tokens": 10, "input_basis": "includes_cache",
        "output_tokens": 2, "output_basis": "includes_reasoning", "observation_point": "gateway"},
    "deployment_attributes": {"user.id": "ada@example.com"}}"#;

fn project(input: &str) -> (Failure, String) {
    // SAFETY: a valid slice and output.
    call(|out| unsafe { ddog_ai_usage_project(cs(input), out) })
}

#[test]
fn project_round_trip() {
    let (error, text) = project(ATTEMPT);
    assert_eq!(error, None);
    let value: Value = serde_json::from_str(&text).unwrap();
    let series = value["series"].as_array().unwrap();
    assert!(series.iter().any(
        |s| s["name"] == "gen_ai.client.inference.usage.input_tokens"
            && s["type"] == "rate"
            && s["interval"] == 10
            && s["value"] == 1.0
    ));
    // Every Counter is a Rate over 10 s, and only a Distribution has no
    // interval.
    assert!(series.iter().all(|s| match s["type"].as_str() {
        Some("rate") => s["interval"] == 10,
        Some("distribution") => s.get("interval").is_none(),
        _ => false,
    }));
    let tags = series[0]["tags"].as_array().unwrap();
    assert!(
        tags.iter()
            .any(|t| t.as_str().unwrap().starts_with("user.id:ada")),
        "{tags:?}"
    );
    assert_eq!(value["issues"], json!([]));

    // `metrics` keeps only the named metrics.
    let mut filtered: Value = serde_json::from_str(ATTEMPT).unwrap();
    filtered["metrics"] = json!(["gen_ai.client.inference.duration"]);
    let (error, text) = project(&filtered.to_string());
    assert_eq!(error, None);
    let value: Value = serde_json::from_str(&text).unwrap();
    assert!(value["series"].as_array().unwrap().iter().all(|s| {
        s["name"]
            .as_str()
            .unwrap()
            .starts_with("gen_ai.client.inference.duration")
    }));
}

#[test]
fn rejections_carry_stable_codes_and_an_empty_output() {
    for (input, expected) in [
        (
            r#"{"profile": "gen_ai.client.provider_attempt@0.1.0", "observation": {}}"#,
            AiUsageErrorCode::RequiredFieldMissing,
        ),
        ("not json", AiUsageErrorCode::FieldTypeInvalid),
        (r#"{"profile": "nope@1"}"#, AiUsageErrorCode::ProfileInvalid),
        (r#"{}"#, AiUsageErrorCode::RequiredFieldMissing),
    ] {
        let (error, text) = project(input);
        let (code, message) = error.unwrap();
        assert_eq!(code, expected, "{input}");
        assert!(!message.is_empty());
        assert_eq!(text, "", "{input}");
    }
    // Bytes that are not UTF-8.
    let bytes = [0xff, 0xfe];
    // SAFETY: a valid slice and output.
    let (error, _) =
        call(|out| unsafe { ddog_ai_usage_project(CharSlice::from_bytes(&bytes), out) });
    assert_eq!(error.unwrap().0, AiUsageErrorCode::FieldTypeInvalid);
}

#[test]
fn every_code_names_itself() {
    let (error, _) = project(r#"{"profile": "nope@1"}"#);
    assert_eq!(error.unwrap().0, AiUsageErrorCode::ProfileInvalid);
    // SAFETY: a valid slice and output.
    let error = unsafe { ddog_ai_usage_project(cs("{}"), ptr::null_mut()) }.unwrap();
    assert_eq!(error.code, AiUsageErrorCode::Internal);
    // SAFETY: a static NUL-terminated string.
    let name = unsafe { CStr::from_ptr(ddog_ai_usage_error_code_name(Some(&error))) };
    assert_eq!(name, c"internal");
    ddog_ai_usage_error_free(Some(error));
    ddog_ai_usage_error_free(None);
}

#[test]
fn null_pointers_and_released_handles_are_internal() {
    let null_slice: CharSlice<'_> =
        // SAFETY: a null pointer with a length, which the library refuses.
        unsafe { CharSlice::from_raw_parts(ptr::null(), 4) };
    // SAFETY: null and invalid pointers are checked before anything is read
    // or written.
    unsafe {
        assert_eq!(
            code(ddog_ai_usage_project(cs("{}"), ptr::null_mut())),
            Some(AiUsageErrorCode::Internal)
        );
        let (error, text) = call(|out| ddog_ai_usage_project(null_slice, out));
        assert_eq!(error.unwrap().0, AiUsageErrorCode::Internal);
        assert_eq!(text, "");
        assert_eq!(
            code(ddog_ai_usage_batch_new(ptr::null_mut())),
            Some(AiUsageErrorCode::Internal)
        );
        let input = format!("[{ATTEMPT}]");
        let (error, _) = call(|out| ddog_ai_usage_batch_add_request(ptr::null(), cs(&input), out));
        assert_eq!(error.unwrap().0, AiUsageErrorCode::Internal);
        assert!(ddog_ai_usage_batch_is_empty(ptr::null()));
        assert_eq!(ddog_ai_usage_batch_len(ptr::null()), 0);
        ddog_ai_usage_batch_drop(ptr::null_mut());
        assert_eq!(
            code(ddog_ai_usage_stream_feed(ptr::null_mut(), bs(b""))),
            Some(AiUsageErrorCode::Internal)
        );
        ddog_ai_usage_stream_drop(ptr::null_mut());
        ddog_StringWrapper_drop(None);
        assert!(!ddog_ai_usage_is_service_tier(null_slice));

        // A released batch: every call sees a null handle, and releasing it
        // again is a no-op.
        let mut batch = new_batch();
        ddog_ai_usage_batch_drop(&mut batch);
        ddog_ai_usage_batch_drop(&mut batch);
        let (error, _) = call(|out| ddog_ai_usage_batch_add_request(&batch, cs(&input), out));
        assert_eq!(error.unwrap().0, AiUsageErrorCode::Internal);
        assert!(ddog_ai_usage_batch_is_empty(&batch));

        // A released stream.
        let mut stream = new_stream("openai_chat", 0);
        ddog_ai_usage_stream_drop(&mut stream);
        ddog_ai_usage_stream_drop(&mut stream);
        assert_eq!(
            code(ddog_ai_usage_stream_feed(&mut stream, bs(b"data: {}\n"))),
            Some(AiUsageErrorCode::Internal)
        );
        assert_eq!(ddog_ai_usage_stream_outcome(&stream), Outcome::Open);
    }
}

#[test]
fn batch_to_dogstatsd_datagrams() {
    let mut batch = new_batch();
    // SAFETY: a live batch, valid slices and outputs.
    unsafe {
        assert!(ddog_ai_usage_batch_is_empty(&batch));
        assert_eq!(ddog_ai_usage_batch_len(&batch), 0);
        let mut one_len = 0;
        let request = format!("[{ATTEMPT}]");
        for i in 0..3 {
            let value = ok_json(|out| ddog_ai_usage_batch_add_request(&batch, cs(&request), out));
            assert_eq!(value, json!({"issues": [[]], "dropped": false}));
            // Counters are summed into their series; each Histogram sample
            // is a point of its own.
            let len = ddog_ai_usage_batch_len(&batch);
            if i == 0 {
                one_len = len;
                assert!(len > 0);
            } else {
                assert!(len > one_len);
            }
        }
        let rejected = r#"[{"profile": "nope@1"}]"#;
        let (error, text) = call(|out| ddog_ai_usage_batch_add_request(&batch, cs(rejected), out));
        assert_eq!(error.unwrap().0, AiUsageErrorCode::ProfileInvalid);
        assert_eq!(text, "");
        assert!(!ddog_ai_usage_batch_is_empty(&batch));

        let mut dropped = 9;
        let (error, text) = call(|out| {
            ddog_ai_usage_batch_take_dogstatsd_text(
                &batch,
                cs("env:prod,version:1.2"),
                512,
                &mut dropped,
                out,
            )
        });
        assert_eq!((error, dropped), (None, 0));
        let datagrams: Vec<&str> = text.split('\0').collect();
        assert!(datagrams.len() > 1);
        let lines: Vec<String> = datagrams
            .iter()
            .flat_map(|d| {
                assert!(d.len() <= 512, "{d}");
                d.lines().map(str::to_string).collect::<Vec<_>>()
            })
            .collect();
        assert!(lines.iter().all(|l| l.ends_with(",env:prod,version:1.2")));
        // Three attempts summed into one count.
        assert!(
            lines
                .iter()
                .any(|l| { l.starts_with("gen_ai.client.inference.usage.input_tokens:30|c|#") })
        );
        assert!(ddog_ai_usage_batch_is_empty(&batch));
        assert_eq!(ddog_ai_usage_batch_len(&batch), 0);

        let taken = call(|out| {
            ddog_ai_usage_batch_take_dogstatsd_text(
                &batch,
                CharSlice::empty(),
                UDP_MAX_PAYLOAD,
                ptr::null_mut(),
                out,
            )
        });
        assert_eq!(taken, (None, String::new()));
        ddog_ai_usage_batch_drop(&mut batch);
    }
}

fn parse(format: &str, options: u32, body: &str) -> (Failure, String) {
    // SAFETY: valid slices and output.
    call(|out| unsafe {
        ddog_ai_usage_parse_provider_response_with(
            cs(format),
            false,
            options,
            bs(body.as_bytes()),
            out,
        )
    })
}

#[test]
fn parse_provider_response_round_trip() {
    let body = r#"{"model": "claude-haiku-4-5", "usage": {"input_tokens": 3, "output_tokens": 1}}"#;
    let value = ok_json(|out| {
        // SAFETY: valid slices and output.
        unsafe {
            ddog_ai_usage_parse_provider_response(cs("anthropic"), false, bs(body.as_bytes()), out)
        }
    });
    assert_eq!(
        value,
        json!({
            "usage": {"input_tokens": 3, "input_basis": "excludes_cache",
                "output_tokens": 1, "output_basis": "includes_reasoning"},
            "response_model": "claude-haiku-4-5", "service_tier": null,
            "outcome": "completed", "error_name": null, "error_type": null,
            "error_code": null, "error_message": null, "issues": [],
        })
    );
    // A tier outside the grammar is not kept, and the issue says why.
    let tiered = r#"{"service_tier": "priority tier", "usage": {"prompt_tokens": 1}}"#;
    let (error, text) = parse("openai_chat", 0, tiered);
    assert_eq!(error, None);
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["service_tier"], Value::Null);
    assert_eq!(value["issues"], json!(["service_tier_dropped"]));

    let (error, text) = parse("no_such_format", 0, body);
    assert_eq!(error.unwrap().0, AiUsageErrorCode::ValueNotAllowed);
    assert_eq!(text, "");
}

#[test]
fn stream_round_trip() {
    let chunks: [&[u8]; 4] = [
        b"event: error\r",
        b"\ndata: {\"type\":\"error\",\"error\":{\"type\":\"rate_",
        b"limit_error\",\"message\":\"Slow\\ndown\"}}\r\n\r\n",
        b"",
    ];
    let mut stream = Handle::empty();
    // SAFETY: `stream` is written by `stream_new` and used until taken.
    unsafe {
        assert_eq!(
            code(ddog_ai_usage_stream_new(
                CharSlice::empty(),
                false,
                &mut stream
            )),
            None
        );
        for chunk in chunks {
            assert_eq!(
                code(ddog_ai_usage_stream_feed(&mut stream, bs(chunk))),
                None
            );
        }
        // The outcome is known before the stream is taken, with no output.
        assert_eq!(ddog_ai_usage_stream_outcome(&stream), Outcome::Failed);
        let value = ok_json(|out| ddog_ai_usage_stream_take(&mut stream, out));
        assert_eq!(value["outcome"], "failed");
        assert_eq!(value["error_name"], "rate_limit_error");
        assert_eq!(value["error_type"], "rate_limit_error");
        assert_eq!(value["error_message"], "Slow down");
        assert_eq!(value["error_code"], Value::Null);
        assert_eq!(ddog_ai_usage_stream_outcome(ptr::null()), Outcome::Open);
        // Taking released the stream: a second take is an error, and a drop
        // a no-op.
        let (error, _) = call(|out| ddog_ai_usage_stream_take(&mut stream, out));
        assert_eq!(error.unwrap().0, AiUsageErrorCode::Internal);
        ddog_ai_usage_stream_drop(&mut stream);

        // An unknown format is rejected, with its code, and writes no handle.
        let mut stream = Handle::empty();
        assert_eq!(
            code(ddog_ai_usage_stream_new(cs("bogus"), false, &mut stream)),
            Some(AiUsageErrorCode::ValueNotAllowed)
        );
        assert_eq!(ddog_ai_usage_stream_outcome(&stream), Outcome::Open);
    }
}

fn text_of(
    f: impl FnOnce(*mut StringWrapper) -> Option<Box<AiUsageError>>,
) -> Result<String, AiUsageErrorCode> {
    match call(f) {
        (None, text) => Ok(text),
        (Some((code, _)), text) => {
            assert_eq!(text, "");
            Err(code)
        }
    }
}

#[test]
fn vocabularies() {
    use AiUsageErrorCode::NoMatch;
    // SAFETY: valid slices and outputs.
    unsafe {
        assert_eq!(
            text_of(|out| ddog_ai_usage_provider_name(cs("bedrock"), out)),
            Ok("aws.bedrock".to_string())
        );
        assert_eq!(
            text_of(|out| ddog_ai_usage_provider_name(cs("1"), out)),
            Err(NoMatch)
        );
        assert_eq!(
            text_of(|out| ddog_ai_usage_error_type(cs("RateLimitError"), 0, out)),
            Ok("rate_limit_error".to_string())
        );
        assert_eq!(
            text_of(|out| ddog_ai_usage_error_type(CharSlice::empty(), 503, out)),
            Ok("internal_server_error".to_string())
        );
        assert_eq!(
            text_of(|out| ddog_ai_usage_error_type(CharSlice::empty(), 200, out)),
            Err(NoMatch)
        );
        assert_eq!(
            text_of(|out| ddog_ai_usage_datadog_tag_value(cs("Hello World"), out)),
            Ok("hello_world".to_string())
        );

        assert!(ddog_ai_usage_is_service_tier(cs("priority")));
        assert!(ddog_ai_usage_is_person_identifier(cs("ada@example.com")));
        assert!(ddog_ai_usage_is_deployment_value(cs("ada@example.com")));
        assert!(!ddog_ai_usage_is_person_identifier(cs("svc account")));
        assert!(!ddog_ai_usage_is_deployment_value(cs("svc account")));
    }
}

fn provider_ids(input: Value) -> Value {
    let input = input.to_string();
    // SAFETY: a valid slice and output.
    ok_json(|out| unsafe { ddog_ai_usage_provider_ids(cs(&input), out) })
}

#[test]
fn provider_ids_from_headers_arns_and_urls() {
    assert_eq!(
        provider_ids(json!({"provider_name": "openai",
            "headers": {"OpenAI-Organization": "acme", "openai-project": "proj_1"}})),
        json!({"trajectory.request.provider_account_id": "acme",
            "trajectory.request.project_id": "proj_1"})
    );
    assert_eq!(
        provider_ids(json!({"provider_name": "aws.bedrock",
            "models": ["arn:aws:bedrock:us-east-1:123456789012:inference-profile/x"]})),
        json!({"trajectory.request.provider_account_id": "123456789012"})
    );
    // The Vertex AI project an upstream URL bills, when no header names one.
    assert_eq!(
        provider_ids(json!({"upstream_url":
            "https://us-east5-aiplatform.googleapis.com/v1/projects/p-1/locations/x"})),
        json!({"trajectory.request.project_id": "p-1"})
    );
    // Without the provider, every known header and the ARNs are tried.
    assert_eq!(
        provider_ids(
            json!({"headers": {"OpenAI-Organization": "acme", "openai-project": "proj_1"}})
        ),
        json!({"trajectory.request.provider_account_id": "acme",
            "trajectory.request.project_id": "proj_1"})
    );
    assert_eq!(
        provider_ids(json!({"provider_name": "",
            "models": ["arn:aws:bedrock:us-east-1:123456789012:inference-profile/x"]})),
        json!({"trajectory.request.provider_account_id": "123456789012"})
    );
}

fn headers(provider: &str) -> Result<Value, AiUsageErrorCode> {
    // SAFETY: a valid slice and output.
    text_of(|out| unsafe { ddog_ai_usage_provider_id_headers(cs(provider), out) })
        .map(|text| serde_json::from_str(&text).unwrap())
}

#[test]
fn provider_id_headers() {
    assert_eq!(
        headers("openai"),
        Ok(json!(["openai-organization", "openai-project"]))
    );
    assert_eq!(
        headers("anthropic"),
        Ok(json!(["anthropic-organization-id"]))
    );
    assert_eq!(headers("aws.bedrock"), Ok(json!([])));
    assert_eq!(
        headers(""),
        Ok(json!([
            "anthropic-organization-id",
            "openai-organization",
            "openai-project"
        ]))
    );
    // A provider name as text or as a JSON string; other JSON is rejected.
    assert_eq!(
        headers("\"openai\""),
        Ok(json!(["openai-organization", "openai-project"]))
    );
    assert_eq!(
        headers("{\"provider\": 1}"),
        Err(AiUsageErrorCode::FieldTypeInvalid)
    );
    // Every header listed is one the id extraction reads.
    for (header, value) in [
        ("anthropic-organization-id", "org-1"),
        ("openai-organization", "acme"),
        ("openai-project", "proj_1"),
    ] {
        let provider = if header.starts_with("anthropic") {
            "anthropic"
        } else {
            "openai"
        };
        let ids = provider_ids(json!({"provider_name": provider, "headers": {header: value}}));
        assert_eq!(ids.as_object().unwrap().len(), 1, "{header}: {ids}");
    }
}

#[test]
fn outputs_release_once() {
    let mut out = StringWrapper::from(String::new());
    // SAFETY: a valid slice and output; the output is released twice, the
    // second time a no-op.
    unsafe {
        assert_eq!(code(ddog_ai_usage_project(cs(ATTEMPT), &mut out)), None);
        assert!(!out.as_ref().is_empty());
        ddog_StringWrapper_drop(Some(&mut out));
        ddog_StringWrapper_drop(Some(&mut out));
    }
    assert_eq!(out.as_ref(), "");
}

fn add(batch: &Handle<Batch>, request: &str) -> Value {
    // SAFETY: a live batch, a valid slice and output.
    ok_json(|out| unsafe { ddog_ai_usage_batch_add_request(batch, cs(request), out) })
}

fn take_text(batch: &Handle<Batch>) -> String {
    // SAFETY: a live batch, valid slices and output.
    let (error, text) = call(|out| unsafe {
        ddog_ai_usage_batch_take_dogstatsd_text(
            batch,
            CharSlice::empty(),
            UDS_MAX_PAYLOAD,
            ptr::null_mut(),
            out,
        )
    });
    assert_eq!(error, None);
    text
}

#[test]
fn a_bounded_batch_drops_and_counts() {
    let request = format!("[{ATTEMPT}]");
    let mut batch = new_batch();
    // SAFETY: a live batch until dropped.
    unsafe {
        add(&batch, &request);
        let one = ddog_ai_usage_batch_len(&batch);
        ddog_ai_usage_batch_set_max_len(&batch, one);
        assert_eq!(
            add(&batch, &request),
            json!({"issues": [[]], "dropped": true})
        );
        assert_eq!(ddog_ai_usage_batch_len(&batch), one);
        assert_eq!(ddog_ai_usage_batch_dropped(&batch), 1);
        // Taking keeps the bound and the count.
        take_text(&batch);
        assert_eq!(ddog_ai_usage_batch_len(&batch), 0);
        assert_eq!(ddog_ai_usage_batch_dropped(&batch), 1);
        assert_eq!(add(&batch, &request)["dropped"], false);
        assert_eq!(add(&batch, &request)["dropped"], true);
        assert_eq!(ddog_ai_usage_batch_dropped(&batch), 2);
        // 0 lifts the bound.
        ddog_ai_usage_batch_set_max_len(&batch, 0);
        assert_eq!(add(&batch, &request)["dropped"], false);
        ddog_ai_usage_batch_set_max_len(ptr::null(), 1);
        assert_eq!(ddog_ai_usage_batch_dropped(ptr::null()), 0);
        ddog_ai_usage_batch_drop(&mut batch);
    }
}

#[test]
fn known_error_types_and_metrics() {
    // SAFETY: valid slices and outputs.
    unsafe {
        assert_eq!(
            text_of(|out| ddog_ai_usage_error_type(cs("SomethingElse"), 0, out)),
            Ok("api_error".to_string())
        );
        assert_eq!(
            text_of(|out| ddog_ai_usage_error_type_known(cs("SomethingElse"), 0, out)),
            Err(AiUsageErrorCode::NoMatch)
        );
        assert_eq!(
            text_of(|out| ddog_ai_usage_error_type_known(cs("api_error"), 0, out)),
            Ok("api_error".to_string())
        );
        assert_eq!(
            text_of(|out| ddog_ai_usage_error_type(cs("404"), 500, out)),
            Ok("internal_server_error".to_string())
        );

        let profile = "gen_ai.client.provider_streaming@0.1.0";
        assert_eq!(
            ok_json(|out| ddog_ai_usage_known_metrics(cs(profile), out)),
            json!(["gen_ai.client.inference.time_per_output_chunk"])
        );
        let all = ok_json(|out| ddog_ai_usage_known_metrics(CharSlice::empty(), out));
        let all = all.as_array().unwrap();
        assert!(all.len() > 10);
        assert!(all.contains(&json!("trajectory.gen_ai.gateway.request.duration")));
        assert_eq!(
            text_of(|out| ddog_ai_usage_known_metrics(cs("nope@1"), out)),
            Err(AiUsageErrorCode::ProfileInvalid)
        );
    }
}

#[test]
fn stream_events_split_by_the_caller() {
    let mut stream = new_stream("openai_chat", 0);
    // SAFETY: `stream` is used until taken; valid slices and output.
    unsafe {
        let data = r#"{"model": "gpt-4o", "usage": {"prompt_tokens": 4, "completion_tokens": 2}}"#;
        assert_eq!(
            code(ddog_ai_usage_stream_feed_event(
                &mut stream,
                CharSlice::empty(),
                cs(data)
            )),
            None
        );
        assert_eq!(
            code(ddog_ai_usage_stream_feed_event(
                &mut stream,
                CharSlice::empty(),
                cs("[DONE]")
            )),
            None
        );
        let value = ok_json(|out| ddog_ai_usage_stream_take(&mut stream, out));
        assert_eq!(value["outcome"], "completed");
        assert_eq!(value["response_model"], "gpt-4o");
        assert_eq!(value["usage"]["input_tokens"], 4);
        assert_eq!(
            code(ddog_ai_usage_stream_feed_event(
                ptr::null_mut(),
                CharSlice::empty(),
                CharSlice::empty()
            )),
            Some(AiUsageErrorCode::Internal)
        );
    }
}

#[test]
fn stream_take_reports_and_releases() {
    let chunk = b"data: {\"model\": \"gpt-4o\", \"usage\": {\"prompt_tokens\": 4, \"completion_tokens\": 1}}\n\ndata: [DONE]";
    let mut stream = new_stream("openai_chat", 0);
    // SAFETY: `stream` is used until taken; valid slices and outputs.
    unsafe {
        assert_eq!(
            code(ddog_ai_usage_stream_feed(&mut stream, bs(chunk))),
            None
        );
        let value = ok_json(|out| ddog_ai_usage_stream_take(&mut stream, out));
        assert_eq!(value["outcome"], "completed");
        assert_eq!(value["usage"]["input_tokens"], 4);
        let (error, text) = call(|out| ddog_ai_usage_stream_take(ptr::null_mut(), out));
        assert_eq!(error.unwrap().0, AiUsageErrorCode::Internal);
        assert_eq!(text, "");
        // A take with a null output still releases the stream.
        let mut stream = new_stream("openai_chat", 0);
        assert_eq!(
            code(ddog_ai_usage_stream_take(&mut stream, ptr::null_mut())),
            Some(AiUsageErrorCode::Internal)
        );
        assert_eq!(
            code(ddog_ai_usage_stream_feed(&mut stream, bs(chunk))),
            Some(AiUsageErrorCode::Internal)
        );
    }
}

const BREAKDOWN: &str = r#"{"profile": "trajectory.gen_ai.client.token_breakdown@0.1.0",
    "observation": {"operation_name": "chat", "provider_name": "anthropic",
        "input_tokens": 100, "input_basis": "excludes_cache", "output_tokens": 50,
        "output_basis": "includes_reasoning", "cache_read_input_tokens": 400,
        "cache_write_input_tokens": 20, "reasoning_output_tokens": 10,
        "observation_point": "gateway"}}"#;

#[test]
fn a_request_is_added_whole_or_not_at_all() {
    let request = format!("[{ATTEMPT}, {BREAKDOWN}]");
    let mut batch = new_batch();
    // SAFETY: a live batch until dropped; valid slices and outputs.
    unsafe {
        let value = add(&batch, &request);
        assert_eq!(value["dropped"], false);
        assert_eq!(value["issues"].as_array().unwrap().len(), 2);
        let one = ddog_ai_usage_batch_len(&batch);
        take_text(&batch);

        // Room for the attempt alone: neither it nor its breakdown is kept.
        ddog_ai_usage_batch_set_max_len(&batch, one - 1);
        assert_eq!(add(&batch, &request)["dropped"], true);
        assert!(ddog_ai_usage_batch_is_empty(&batch));
        assert_eq!(ddog_ai_usage_batch_dropped(&batch), 2);
        // Quietly, with a null output.
        ddog_ai_usage_batch_set_max_len(&batch, 0);
        assert_eq!(
            code(ddog_ai_usage_batch_add_request(
                &batch,
                cs(&request),
                ptr::null_mut()
            )),
            None
        );
        assert_eq!(ddog_ai_usage_batch_len(&batch), one);

        // A rejected document rejects the request and adds nothing.
        let bad = format!(r#"[{ATTEMPT}, {{"profile": "nope@1"}}]"#);
        let (error, text) = call(|out| ddog_ai_usage_batch_add_request(&batch, cs(&bad), out));
        let (code_, message) = error.unwrap();
        assert_eq!(code_, AiUsageErrorCode::ProfileInvalid);
        assert!(message.starts_with("document 1:"), "{message}");
        assert_eq!(text, "");
        assert_eq!(ddog_ai_usage_batch_len(&batch), one);
        // A rejection with a null output still returns its error.
        assert_eq!(
            code(ddog_ai_usage_batch_add_request(
                &batch,
                cs(&bad),
                ptr::null_mut()
            )),
            Some(AiUsageErrorCode::ProfileInvalid)
        );
        // A single document is not a request.
        let (error, _) = call(|out| ddog_ai_usage_batch_add_request(&batch, cs(ATTEMPT), out));
        assert!(error.is_some());
        ddog_ai_usage_batch_drop(&mut batch);
    }
}

#[test]
fn operation_names_cross_the_boundary() {
    // SAFETY: valid slices and outputs.
    unsafe {
        assert_eq!(
            text_of(|out| ddog_ai_usage_operation_name(cs("/v1/chat/completions?x=1"), out)),
            Ok("chat".to_string())
        );
        assert_eq!(
            text_of(|out| ddog_ai_usage_operation_name(cs("/v1/models"), out)),
            Err(AiUsageErrorCode::NoMatch)
        );
    }
}

#[test]
fn a_batch_taken_as_intake_series() {
    let request = format!("[{ATTEMPT}, {BREAKDOWN}]");
    let mut batch = new_batch();
    // SAFETY: a live batch until dropped; valid slices and outputs.
    unsafe {
        ddog_ai_usage_batch_set_writer(&batch, cs("gateway-7f9"), 7);
        for _ in 0..3 {
            assert_eq!(
                code(ddog_ai_usage_batch_add_request(
                    &batch,
                    cs(&request),
                    ptr::null_mut()
                )),
                None
            );
        }
        let value = ok_json(|out| ddog_ai_usage_batch_take_intake(&batch, 1_700_000_000, out));
        assert_eq!(value["timestamp"], 1_700_000_000);
        assert_eq!(value["interval"], 10);
        assert_eq!(value["dropped"], 0);
        let series = value["series"].as_array().unwrap();
        let input = series
            .iter()
            .find(|s| s["metric"] == "gen_ai.client.inference.usage.input_tokens")
            .unwrap();
        // Three attempts of 10 input tokens over 10 s, as a Rate.
        assert_eq!(input["value"], 3.0);
        assert!(
            series
                .iter()
                .all(|s| s["type"] == "rate" && s["interval"] == 10),
            "{series:?}"
        );
        let tags: Vec<&str> = input["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        assert!(
            tags.contains(&"host.name:gateway-7f9") && tags.contains(&"process.pid:7"),
            "{tags:?}"
        );
        let distributions = value["distributions"].as_array().unwrap();
        assert!(!distributions.is_empty());
        assert!(
            distributions
                .iter()
                .all(|d| !d["sketch"].as_str().unwrap().is_empty())
        );
        // The next take never reuses the second.
        let value = ok_json(|out| ddog_ai_usage_batch_take_intake(&batch, 1_700_000_000, out));
        assert_eq!(value["timestamp"], 1_700_000_001);
        assert_eq!(value["series"], json!([]));
        ddog_ai_usage_batch_set_writer(ptr::null(), cs("gateway-7f9"), 1);
        ddog_ai_usage_batch_drop(&mut batch);
    }
}

#[test]
fn batch_metrics() {
    let request = format!("[{ATTEMPT}]");
    let mut batch = new_batch();
    // SAFETY: a live batch until dropped; valid slices and outputs.
    unsafe {
        let set = |input: &str| code(ddog_ai_usage_batch_set_metrics(&batch, cs(input)));
        let add_quietly = || {
            assert_eq!(
                code(ddog_ai_usage_batch_add_request(
                    &batch,
                    cs(&request),
                    ptr::null_mut()
                )),
                None
            );
        };
        assert_eq!(
            set(r#"["gen_ai.client.inference.usage.input_tokens"]"#),
            None
        );
        assert_eq!(set(r#"{"a": 1}"#), Some(AiUsageErrorCode::FieldTypeInvalid));
        add_quietly();
        let lines = take_text(&batch);
        assert_eq!(lines.lines().count(), 1, "{lines}");
        assert!(lines.starts_with("gen_ai.client.inference.usage.input_tokens:10|c|#"));
        // `[]` keeps no metric; `null` keeps every metric again, as does an
        // empty slice.
        assert_eq!(set("[]"), None);
        add_quietly();
        assert!(ddog_ai_usage_batch_is_empty(&batch));
        assert_eq!(set("null"), None);
        add_quietly();
        assert!(ddog_ai_usage_batch_len(&batch) > 1);
        take_text(&batch);
        assert_eq!(set("[]"), None);
        assert_eq!(set(""), None);
        add_quietly();
        assert!(ddog_ai_usage_batch_len(&batch) > 1);
        assert_eq!(
            code(ddog_ai_usage_batch_set_metrics(ptr::null(), cs("[]"))),
            Some(AiUsageErrorCode::Internal)
        );
        ddog_ai_usage_batch_drop(&mut batch);
    }
}

/// A batch handle sent to other threads, as a C caller shares it.
struct SharedHandle(*const Handle<Batch>);

impl SharedHandle {
    fn get(&self) -> *const Handle<Batch> {
        self.0
    }
}

// SAFETY: a batch is safe to use from several threads at once.
unsafe impl Send for SharedHandle {}
// SAFETY: as above.
unsafe impl Sync for SharedHandle {}

#[test]
fn a_batch_is_shared_between_threads() {
    let request = format!("[{ATTEMPT}]");
    let mut batch = new_batch();
    // A handle is a plain pointer, which C callers share between threads.
    let shared = SharedHandle(&batch);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                let handle = shared.get();
                for _ in 0..25 {
                    // SAFETY: a live batch, shared; a valid slice.
                    let error = unsafe {
                        ddog_ai_usage_batch_add_request(handle, cs(&request), ptr::null_mut())
                    };
                    assert_eq!(code(error), None);
                }
            });
        }
    });
    let value = ok_json(|out| {
        // SAFETY: a live batch and a valid output.
        unsafe { ddog_ai_usage_batch_take_intake(&batch, 1_700_000_000, out) }
    });
    let input = value["series"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["metric"] == "gen_ai.client.inference.usage.input_tokens")
        .unwrap()
        .clone();
    // 100 attempts of 10 input tokens over 10 s.
    assert_eq!(input["value"], 100.0);
    // SAFETY: released once.
    unsafe { ddog_ai_usage_batch_drop(&mut batch) };
}

#[test]
fn inclusive_totals_across_the_boundary() {
    let body = r#"{"model": "claude-sonnet-4-5", "usage": {"input_tokens": 10, "output_tokens": 5,
        "cache_read_input_tokens": 100}}"#;
    let (error, text) = parse("anthropic", INCLUSIVE_TOTALS, body);
    assert_eq!(error, None);
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["usage"]["input_tokens"], 110);
    assert_eq!(value["usage"]["input_basis"], "includes_cache");
    let (error, _) = parse("anthropic", 8, body);
    assert_eq!(error.unwrap().0, AiUsageErrorCode::FieldTypeInvalid);

    let mut stream = new_stream("anthropic", INCLUSIVE_TOTALS);
    let chunk = format!("data: {}\n\n", body.replace('\n', " "));
    // SAFETY: `stream` is used until taken; valid slices and output.
    unsafe {
        assert_eq!(
            code(ddog_ai_usage_stream_feed(&mut stream, bs(chunk.as_bytes()))),
            None
        );
        let value = ok_json(|out| ddog_ai_usage_stream_take(&mut stream, out));
        assert_eq!(value["usage"]["input_tokens"], 110);
        let mut stream = Handle::empty();
        assert_eq!(
            code(ddog_ai_usage_stream_new_with(
                cs("anthropic"),
                false,
                8,
                &mut stream
            )),
            Some(AiUsageErrorCode::FieldTypeInvalid)
        );
    }
}

#[test]
fn the_abi_version_is_stated() {
    assert_eq!(ddog_ai_usage_abi_version(), 2);
    assert_eq!(ddog_ai_usage_abi_version(), ABI_VERSION);
}
