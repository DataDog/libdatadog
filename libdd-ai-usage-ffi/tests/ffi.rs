// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The C interface as a caller sees it: JSON in and out, statuses, handles,
//! and buffers. The projection and parsing rules are libdd-ai-usage's and
//! are tested there.

use std::ptr;

use dd_ai_usage_ffi::*;
use serde_json::{Value, json};

fn empty() -> Buffer {
    Buffer {
        ptr: ptr::null_mut(),
        len: 0,
    }
}

/// The buffer's bytes as text, released.
fn take(mut out: Buffer) -> String {
    let text = if out.ptr.is_null() {
        String::new()
    } else {
        // SAFETY: the library wrote `len` bytes at `ptr`.
        let bytes = unsafe { std::slice::from_raw_parts(out.ptr, out.len) };
        String::from_utf8(bytes.to_vec()).unwrap()
    };
    // SAFETY: released once.
    unsafe { ddog_ai_usage_buffer_drop(&mut out) };
    assert!(out.ptr.is_null());
    text
}

fn json_of(out: Buffer) -> Value {
    serde_json::from_str(&take(out)).unwrap()
}

const ATTEMPT: &str = r#"{"profile": "gen_ai.client.provider_attempt@0.1.0",
    "observation": {"operation_name": "chat", "provider_name": "openai",
        "duration_seconds": 0.5, "streaming": false, "input_tokens": 10, "input_basis": "includes_cache",
        "output_tokens": 2, "output_basis": "includes_reasoning", "observation_point": "gateway"},
    "deployment_attributes": {"user.id": "ada@example.com"}}"#;

fn project(input: &str) -> (Status, Value) {
    let mut out = empty();
    // SAFETY: valid input and output.
    let status = unsafe { ddog_ai_usage_project(input.as_ptr(), input.len(), &mut out) };
    (status, json_of(out))
}

#[test]
fn project_round_trip() {
    let (status, value) = project(ATTEMPT);
    assert_eq!(status, Status::Ok);
    let series = value["series"].as_array().unwrap();
    assert!(series.iter().any(
        |s| s["name"] == "gen_ai.client.inference.usage.input_tokens"
            && s["type"] == "count"
            && s["value"] == 10.0
    ));
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
    let (status, value) = project(&filtered.to_string());
    assert_eq!(status, Status::Ok);
    assert!(value["series"].as_array().unwrap().iter().all(|s| {
        s["name"]
            .as_str()
            .unwrap()
            .starts_with("gen_ai.client.inference.duration")
    }));
}

#[test]
fn project_rejections_carry_stable_codes() {
    for (input, code) in [
        (
            r#"{"profile": "gen_ai.client.provider_attempt@0.1.0", "observation": {}}"#,
            "required_field_missing",
        ),
        ("not json", "field_type_invalid"),
        (r#"{"profile": "nope@1"}"#, "profile_invalid"),
        (r#"{}"#, "required_field_missing"),
    ] {
        let (status, value) = project(input);
        assert_eq!(status, Status::Rejected, "{input}");
        assert_eq!(value["error"]["code"], code, "{input}");
    }
    // Bytes that are not UTF-8.
    let mut out = empty();
    let bytes = [0xff, 0xfe];
    // SAFETY: valid input and output.
    let status = unsafe { ddog_ai_usage_project(bytes.as_ptr(), bytes.len(), &mut out) };
    assert_eq!(status, Status::Rejected);
    assert_eq!(json_of(out)["error"]["code"], "field_type_invalid");
}

#[test]
fn null_pointers_are_internal() {
    // SAFETY: null pointers are checked before anything is read or written.
    unsafe {
        assert_eq!(
            ddog_ai_usage_project(ptr::null(), 0, ptr::null_mut()),
            Status::Internal
        );
        let mut out = empty();
        assert_eq!(
            ddog_ai_usage_project(ptr::null(), 4, &mut out),
            Status::Internal
        );
        assert_eq!(json_of(out)["error"]["code"], "internal");
        assert_eq!(ddog_ai_usage_batch_new(ptr::null_mut()), Status::Internal);
        let mut out = empty();
        let input = ATTEMPT.as_bytes();
        assert_eq!(
            ddog_ai_usage_batch_add(ptr::null(), input.as_ptr(), input.len(), &mut out),
            Status::Internal
        );
        take(out);
        assert!(ddog_ai_usage_batch_is_empty(ptr::null()));
        assert_eq!(ddog_ai_usage_batch_len(ptr::null()), 0);
        ddog_ai_usage_batch_drop(ptr::null_mut());
        assert_eq!(
            ddog_ai_usage_stream_feed(ptr::null_mut(), ptr::null(), 0),
            Status::Internal
        );
        ddog_ai_usage_stream_drop(ptr::null_mut());
        ddog_ai_usage_buffer_drop(ptr::null_mut());
        assert!(!ddog_ai_usage_is_service_tier(ptr::null(), 3));
    }
}

#[test]
fn batch_to_dogstatsd_datagrams() {
    let mut batch = ptr::null_mut();
    // SAFETY: `batch` is written by `batch_new` and used until dropped.
    unsafe {
        assert_eq!(ddog_ai_usage_batch_new(&mut batch), Status::Ok);
        assert!(ddog_ai_usage_batch_is_empty(batch));
        assert_eq!(ddog_ai_usage_batch_len(batch), 0);
        let mut one_len = 0;
        for i in 0..3 {
            let mut out = empty();
            let status = ddog_ai_usage_batch_add(batch, ATTEMPT.as_ptr(), ATTEMPT.len(), &mut out);
            assert_eq!(status, Status::Ok);
            assert_eq!(json_of(out), json!({"issues": []}));
            // Counters are summed into their series; each Histogram sample
            // is a point of its own.
            let len = ddog_ai_usage_batch_len(batch);
            if i == 0 {
                one_len = len;
                assert!(len > 0);
            } else {
                assert!(len > one_len);
            }
        }
        let rejected = r#"{"profile": "nope@1"}"#;
        let mut out = empty();
        assert_eq!(
            ddog_ai_usage_batch_add(batch, rejected.as_ptr(), rejected.len(), &mut out),
            Status::Rejected
        );
        take(out);
        assert!(!ddog_ai_usage_batch_is_empty(batch));

        let tags = "env:prod,version:1.2";
        let mut out = empty();
        let status =
            ddog_ai_usage_batch_take_dogstatsd(batch, tags.as_ptr(), tags.len(), 256, &mut out);
        assert_eq!(status, Status::Ok);
        let datagrams = json_of(out)["datagrams"].as_array().unwrap().clone();
        assert!(datagrams.len() > 1);
        let lines: Vec<String> = datagrams
            .iter()
            .flat_map(|d| {
                let d = d.as_str().unwrap();
                assert!(d.len() <= 256 || !d.contains('\n'), "{d}");
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
        assert!(ddog_ai_usage_batch_is_empty(batch));
        assert_eq!(ddog_ai_usage_batch_len(batch), 0);

        let mut out = empty();
        let status =
            ddog_ai_usage_batch_take_dogstatsd(batch, ptr::null(), 0, UDP_MAX_PAYLOAD, &mut out);
        assert_eq!(status, Status::Ok);
        assert_eq!(json_of(out), json!({"datagrams": []}));
        ddog_ai_usage_batch_drop(batch);
    }
}

#[test]
fn parse_provider_response_round_trip() {
    let body = r#"{"model": "claude-haiku-4-5", "usage": {"input_tokens": 3, "output_tokens": 1}}"#;
    let format = "anthropic";
    let mut out = empty();
    // SAFETY: valid inputs and output.
    let status = unsafe {
        ddog_ai_usage_parse_provider_response(
            format.as_ptr(),
            format.len(),
            false,
            body.as_ptr(),
            body.len(),
            &mut out,
        )
    };
    assert_eq!(status, Status::Ok);
    assert_eq!(
        json_of(out),
        json!({
            "usage": {"input_tokens": 3, "input_basis": "excludes_cache",
                "output_tokens": 1, "output_basis": "includes_reasoning"},
            "response_model": "claude-haiku-4-5", "service_tier": null,
            "outcome": "completed", "error_name": null, "error_type": null,
        })
    );

    let format = "no_such_format";
    let mut out = empty();
    // SAFETY: valid inputs and output.
    let status = unsafe {
        ddog_ai_usage_parse_provider_response(
            format.as_ptr(),
            format.len(),
            false,
            body.as_ptr(),
            body.len(),
            &mut out,
        )
    };
    assert_eq!(status, Status::Rejected);
    assert_eq!(json_of(out)["error"]["code"], "value_not_allowed");
}

#[test]
fn stream_round_trip() {
    let mut stream = ptr::null_mut();
    let chunks: [&[u8]; 4] = [
        b"event: error\r",
        b"\ndata: {\"type\":\"error\",\"error\":{\"type\":\"rate_",
        b"limit_error\"}}\r\n\r\n",
        b"",
    ];
    // SAFETY: `stream` is written by `stream_new` and used until dropped.
    unsafe {
        assert_eq!(
            ddog_ai_usage_stream_new(ptr::null(), 0, false, &mut stream),
            Status::Ok
        );
        for chunk in chunks {
            assert_eq!(
                ddog_ai_usage_stream_feed(stream, chunk.as_ptr(), chunk.len()),
                Status::Ok
            );
        }
        let mut out = empty();
        assert_eq!(ddog_ai_usage_stream_finish(stream, &mut out), Status::Ok);
        let value = json_of(out);
        assert_eq!(value["outcome"], "failed");
        assert_eq!(value["error_name"], "rate_limit_error");
        assert_eq!(value["error_type"], "rate_limit_error");
        // Finishing does not consume the stream.
        let mut out = empty();
        assert_eq!(ddog_ai_usage_stream_finish(stream, &mut out), Status::Ok);
        take(out);
        ddog_ai_usage_stream_drop(stream);

        let format = "bogus";
        let mut stream = ptr::null_mut();
        assert_eq!(
            ddog_ai_usage_stream_new(format.as_ptr(), format.len(), false, &mut stream),
            Status::Rejected
        );
        assert!(stream.is_null());
    }
}

fn text_of(call: impl FnOnce(*mut Buffer) -> Status) -> (Status, String) {
    let mut out = empty();
    let status = call(&mut out);
    (status, take(out))
}

#[test]
fn vocabularies() {
    // SAFETY: valid inputs and outputs.
    unsafe {
        let name = "bedrock";
        assert_eq!(
            text_of(|out| ddog_ai_usage_provider_name(name.as_ptr(), name.len(), out)),
            (Status::Ok, "aws.bedrock".to_string())
        );
        let name = "1";
        assert_eq!(
            text_of(|out| ddog_ai_usage_provider_name(name.as_ptr(), name.len(), out)),
            (Status::Rejected, String::new())
        );
        let name = "RateLimitError";
        assert_eq!(
            text_of(|out| ddog_ai_usage_error_type(name.as_ptr(), name.len(), 0, out)),
            (Status::Ok, "rate_limit_error".to_string())
        );
        assert_eq!(
            text_of(|out| ddog_ai_usage_error_type(ptr::null(), 0, 503, out)),
            (Status::Ok, "internal_server_error".to_string())
        );
        assert_eq!(
            text_of(|out| ddog_ai_usage_error_type(ptr::null(), 0, 200, out)),
            (Status::Rejected, String::new())
        );
        let value = "Hello World";
        assert_eq!(
            text_of(|out| ddog_ai_usage_datadog_tag_value(value.as_ptr(), value.len(), out)),
            (Status::Ok, "hello_world".to_string())
        );

        let tier = "priority";
        assert!(ddog_ai_usage_is_service_tier(tier.as_ptr(), tier.len()));
        let email = "ada@example.com";
        assert!(ddog_ai_usage_is_person_identifier(
            email.as_ptr(),
            email.len()
        ));
        assert!(ddog_ai_usage_is_deployment_value(
            email.as_ptr(),
            email.len()
        ));
        let service = "svc account";
        assert!(!ddog_ai_usage_is_person_identifier(
            service.as_ptr(),
            service.len()
        ));
        assert!(!ddog_ai_usage_is_deployment_value(
            service.as_ptr(),
            service.len()
        ));
    }
}

#[test]
fn provider_ids_and_packing() {
    let call = |f: unsafe extern "C" fn(*const u8, usize, *mut Buffer) -> Status, input: Value| {
        let input = input.to_string();
        let mut out = empty();
        // SAFETY: valid input and output.
        let status = unsafe { f(input.as_ptr(), input.len(), &mut out) };
        (status, json_of(out))
    };
    assert_eq!(
        call(
            ddog_ai_usage_provider_ids,
            json!({"provider_name": "openai",
                "headers": {"OpenAI-Organization": "acme", "openai-project": "proj_1"}}),
        ),
        (
            Status::Ok,
            json!({"trajectory.request.provider_account_id": "acme",
                "trajectory.request.project_id": "proj_1"})
        )
    );
    assert_eq!(
        call(
            ddog_ai_usage_provider_ids,
            json!({"provider_name": "aws.bedrock",
                "models": ["arn:aws:bedrock:us-east-1:123456789012:inference-profile/x"]}),
        ),
        (
            Status::Ok,
            json!({"trajectory.request.provider_account_id": "123456789012"})
        )
    );
    assert_eq!(
        call(
            ddog_ai_usage_pack_dogstatsd,
            json!({"lines": ["a:1|c|#x:y", "b:1|c|#x:y"], "constant_tags": ["env:prod"],
                "max_payload": 30}),
        ),
        (
            Status::Ok,
            json!({"datagrams": ["a:1|c|#x:y,env:prod", "b:1|c|#x:y,env:prod"]})
        )
    );
    let (status, value) = call(
        ddog_ai_usage_pack_dogstatsd,
        json!({"lines": [1], "max_payload": 30}),
    );
    assert_eq!(status, Status::Rejected);
    assert_eq!(value["error"]["code"], "field_type_invalid");
    let (status, _) = call(
        ddog_ai_usage_pack_dogstatsd,
        json!({"lines": [], "max_payload": 0}),
    );
    assert_eq!(status, Status::Rejected);
}

#[test]
fn provider_id_headers() {
    let headers = |provider: &str| {
        let mut out = empty();
        // SAFETY: valid input and output.
        let status = unsafe {
            ddog_ai_usage_provider_id_headers(provider.as_ptr(), provider.len(), &mut out)
        };
        (status, json_of(out))
    };
    assert_eq!(
        headers("openai"),
        (Status::Ok, json!(["openai-organization", "openai-project"]))
    );
    assert_eq!(
        headers("anthropic"),
        (Status::Ok, json!(["anthropic-organization-id"]))
    );
    assert_eq!(headers("aws.bedrock"), (Status::Ok, json!([])));
    let (status, all) = headers("");
    assert_eq!(status, Status::Ok);
    assert_eq!(
        all,
        json!([
            "anthropic-organization-id",
            "openai-organization",
            "openai-project"
        ])
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
        let input = json!({"provider_name": provider, "headers": {header: value}}).to_string();
        let mut out = empty();
        // SAFETY: valid input and output.
        let status = unsafe { ddog_ai_usage_provider_ids(input.as_ptr(), input.len(), &mut out) };
        assert_eq!(status, Status::Ok);
        let ids = json_of(out);
        assert_eq!(ids.as_object().unwrap().len(), 1, "{header}: {ids}");
    }
}

#[test]
fn buffers_release_once() {
    let mut out = empty();
    // SAFETY: valid input and output.
    unsafe {
        ddog_ai_usage_project(ATTEMPT.as_ptr(), ATTEMPT.len(), &mut out);
        assert!(!out.ptr.is_null());
        ddog_ai_usage_buffer_drop(&mut out);
        // The buffer was left empty, so a second release is a no-op.
        ddog_ai_usage_buffer_drop(&mut out);
    }
    assert!(out.ptr.is_null());
    assert_eq!(out.len, 0);
}
