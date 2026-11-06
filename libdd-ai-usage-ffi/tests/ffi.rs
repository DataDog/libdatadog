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
            assert_eq!(
                json_of(out),
                json!({"issues": [], "dropped": false, "full": false})
            );
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
            ddog_ai_usage_batch_take_dogstatsd(batch, tags.as_ptr(), tags.len(), 512, &mut out);
        assert_eq!(status, Status::Ok);
        let taken = json_of(out);
        assert_eq!(taken["dropped_lines"], 0);
        let datagrams = taken["datagrams"].as_array().unwrap().clone();
        assert!(datagrams.len() > 1);
        let lines: Vec<String> = datagrams
            .iter()
            .flat_map(|d| {
                let d = d.as_str().unwrap();
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
        assert!(ddog_ai_usage_batch_is_empty(batch));
        assert_eq!(ddog_ai_usage_batch_len(batch), 0);

        let mut out = empty();
        let status =
            ddog_ai_usage_batch_take_dogstatsd(batch, ptr::null(), 0, UDP_MAX_PAYLOAD, &mut out);
        assert_eq!(status, Status::Ok);
        assert_eq!(json_of(out), json!({"datagrams": [], "dropped_lines": 0}));
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
            "error_code": null, "error_message": null, "issues": [],
        })
    );
    // A tier outside the grammar is not kept, and the issue says why.
    let tiered = r#"{"service_tier": "priority tier", "usage": {"prompt_tokens": 1}}"#;
    let format = "openai_chat";
    let mut out = empty();
    // SAFETY: valid inputs and output.
    let status = unsafe {
        ddog_ai_usage_parse_provider_response(
            format.as_ptr(),
            format.len(),
            false,
            tiered.as_ptr(),
            tiered.len(),
            &mut out,
        )
    };
    assert_eq!(status, Status::Ok);
    let value = json_of(out);
    assert_eq!(value["service_tier"], Value::Null);
    assert_eq!(value["issues"], json!(["service_tier_dropped"]));

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
        b"limit_error\",\"message\":\"Slow\\ndown\"}}\r\n\r\n",
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
        assert_eq!(value["error_message"], "Slow down");
        assert_eq!(value["error_code"], Value::Null);
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
            json!({"datagrams": ["a:1|c|#x:y,env:prod", "b:1|c|#x:y,env:prod"],
                "dropped_lines": 0})
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

/// The text of a NUL-separated datagram buffer, one string per datagram.
fn datagrams_of(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split('\0').map(str::to_string).collect()
}

fn lines_of(input: &str) -> (Status, String) {
    // SAFETY: valid input and output.
    text_of(|out| unsafe { ddog_ai_usage_dogstatsd_lines(input.as_ptr(), input.len(), out) })
}

#[test]
fn text_lines_and_packing_match_the_json_path() {
    // One observation's lines as text, in a fixed order (by name).
    let (status, text) = lines_of(ATTEMPT);
    assert_eq!(status, Status::Ok);
    assert!(!text.ends_with('\n'));
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.len() > 1);
    let names: Vec<&str> = lines.iter().map(|l| l.split(':').next().unwrap()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert_eq!(lines_of(ATTEMPT).1, text);

    // Packing the text lines gives the datagrams the batch gives.
    let joined = lines.join("\r\n") + "\n\n";
    let tags = "env:prod, version:1.2";
    let mut dropped = usize::MAX;
    // SAFETY: valid inputs and output.
    let (status, packed) = text_of(|out| unsafe {
        ddog_ai_usage_pack_dogstatsd_lines(
            joined.as_ptr(),
            joined.len(),
            tags.as_ptr(),
            tags.len(),
            256,
            &mut dropped,
            out,
        )
    });
    assert_eq!(status, Status::Ok);
    let packed = datagrams_of(&packed);
    assert!(packed.len() > 1);
    for datagram in &packed {
        assert!(datagram.len() <= 256 || !datagram.contains('\n'));
    }
    let mut batch = ptr::null_mut();
    // SAFETY: `batch` is written by `batch_new` and used until dropped.
    unsafe {
        assert_eq!(ddog_ai_usage_batch_new(&mut batch), Status::Ok);
        let mut out = empty();
        ddog_ai_usage_batch_add(batch, ATTEMPT.as_ptr(), ATTEMPT.len(), &mut out);
        take(out);
        let mut batch_dropped = usize::MAX;
        let (status, taken) = text_of(|out| {
            ddog_ai_usage_batch_take_dogstatsd_text(
                batch,
                tags.as_ptr(),
                tags.len(),
                256,
                &mut batch_dropped,
                out,
            )
        });
        assert_eq!(status, Status::Ok);
        assert_eq!(datagrams_of(&taken), packed);
        assert_eq!(batch_dropped, dropped);
        assert_eq!(
            text_of(|out| ddog_ai_usage_batch_take_dogstatsd_text(
                batch,
                ptr::null(),
                0,
                UDP_MAX_PAYLOAD,
                ptr::null_mut(),
                out
            )),
            (Status::Ok, String::new())
        );
        ddog_ai_usage_batch_drop(batch);
    }

    // Rejections carry the JSON error, as the JSON path does.
    let (status, error) = lines_of(r#"{"profile": "nope@1"}"#);
    assert_eq!(status, Status::Rejected);
    let error: Value = serde_json::from_str(&error).unwrap();
    assert_eq!(error["error"]["code"], "profile_invalid");
    // SAFETY: valid input and output.
    let (status, _) = text_of(|out| unsafe {
        ddog_ai_usage_pack_dogstatsd_lines(ptr::null(), 0, ptr::null(), 0, 0, ptr::null_mut(), out)
    });
    assert_eq!(status, Status::Rejected);
    // A line longer than the payload goes alone in its own datagram; one
    // longer than 8192 bytes is dropped and counted.
    let lines = format!(
        "a:1|c\nb:1|c|#t:{}\nc:1|c\nd:1|c|#t:{}",
        "x".repeat(64),
        "y".repeat(9000)
    );
    let mut dropped = 0;
    // SAFETY: valid inputs and output.
    let (status, packed) = text_of(|out| unsafe {
        ddog_ai_usage_pack_dogstatsd_lines(
            lines.as_ptr(),
            lines.len(),
            ptr::null(),
            0,
            32,
            &mut dropped,
            out,
        )
    });
    let alone = format!("b:1|c|#t:{}", "x".repeat(64));
    assert_eq!(
        (status, packed, dropped),
        (Status::Ok, format!("a:1|c\0{alone}\0c:1|c"), 1)
    );
    // The JSON path agrees, and takes the longest line to send.
    let request = json!({
        "lines": lines.split('\n').collect::<Vec<_>>(),
        "max_payload": 32,
    })
    .to_string();
    let mut out = empty();
    // SAFETY: valid input and output.
    let status = unsafe { ddog_ai_usage_pack_dogstatsd(request.as_ptr(), request.len(), &mut out) };
    assert_eq!(status, Status::Ok);
    assert_eq!(
        json_of(out),
        json!({"datagrams": ["a:1|c", alone, "c:1|c"], "dropped_lines": 1})
    );
    let request = json!({
        "lines": lines.split('\n').collect::<Vec<_>>(),
        "max_payload": 32,
        "max_line_bytes": 40,
    })
    .to_string();
    let mut out = empty();
    // SAFETY: valid input and output.
    let status = unsafe { ddog_ai_usage_pack_dogstatsd(request.as_ptr(), request.len(), &mut out) };
    assert_eq!(status, Status::Ok);
    assert_eq!(
        json_of(out),
        json!({"datagrams": ["a:1|c\nc:1|c"], "dropped_lines": 2})
    );
}

#[test]
fn a_bounded_batch_drops_and_counts() {
    let add = |batch: *const Batch| {
        let mut out = empty();
        // SAFETY: a live batch and valid input and output.
        let status =
            unsafe { ddog_ai_usage_batch_add(batch, ATTEMPT.as_ptr(), ATTEMPT.len(), &mut out) };
        assert_eq!(status, Status::Ok);
        json_of(out)
    };
    let mut batch = ptr::null_mut();
    // SAFETY: `batch` is written by `batch_new` and used until dropped.
    unsafe {
        assert_eq!(ddog_ai_usage_batch_new(&mut batch), Status::Ok);
        add(batch);
        let one = ddog_ai_usage_batch_len(batch);
        ddog_ai_usage_batch_set_max_len(batch, one);
        assert_eq!(
            add(batch),
            json!({"issues": [], "dropped": true, "full": false})
        );
        assert_eq!(ddog_ai_usage_batch_len(batch), one);
        assert_eq!(ddog_ai_usage_batch_dropped(batch), 1);
        // Taking keeps the bound and the count.
        let mut out = empty();
        ddog_ai_usage_batch_take_dogstatsd(batch, ptr::null(), 0, UDP_MAX_PAYLOAD, &mut out);
        take(out);
        assert_eq!(ddog_ai_usage_batch_len(batch), 0);
        assert_eq!(ddog_ai_usage_batch_dropped(batch), 1);
        assert_eq!(add(batch)["dropped"], false);
        assert_eq!(add(batch)["dropped"], true);
        assert_eq!(ddog_ai_usage_batch_dropped(batch), 2);
        // 0 lifts the bound.
        ddog_ai_usage_batch_set_max_len(batch, 0);
        assert_eq!(add(batch)["dropped"], false);
        ddog_ai_usage_batch_set_max_len(ptr::null(), 1);
        assert_eq!(ddog_ai_usage_batch_dropped(ptr::null()), 0);
        ddog_ai_usage_batch_drop(batch);
    }
}

#[test]
fn known_error_types_and_metrics() {
    // SAFETY: valid inputs and outputs.
    unsafe {
        let name = "SomethingElse";
        assert_eq!(
            text_of(|out| ddog_ai_usage_error_type(name.as_ptr(), name.len(), 0, out)),
            (Status::Ok, "api_error".to_string())
        );
        assert_eq!(
            text_of(|out| ddog_ai_usage_error_type_known(name.as_ptr(), name.len(), 0, out)),
            (Status::Rejected, String::new())
        );
        let name = "api_error";
        assert_eq!(
            text_of(|out| ddog_ai_usage_error_type_known(name.as_ptr(), name.len(), 0, out)),
            (Status::Ok, "api_error".to_string())
        );
        let name = "404";
        assert_eq!(
            text_of(|out| ddog_ai_usage_error_type(name.as_ptr(), name.len(), 500, out)),
            (Status::Ok, "internal_server_error".to_string())
        );

        let profile = "gen_ai.client.provider_streaming@0.1.0";
        let mut out = empty();
        assert_eq!(
            ddog_ai_usage_known_metrics(profile.as_ptr(), profile.len(), &mut out),
            Status::Ok
        );
        assert_eq!(
            json_of(out),
            json!(["gen_ai.client.inference.time_per_output_chunk"])
        );
        let mut out = empty();
        assert_eq!(
            ddog_ai_usage_known_metrics(ptr::null(), 0, &mut out),
            Status::Ok
        );
        let all = json_of(out);
        let all = all.as_array().unwrap();
        assert!(all.len() > 10);
        assert!(all.contains(&json!("trajectory.gen_ai.gateway.request.duration")));
        let profile = "nope@1";
        let mut out = empty();
        assert_eq!(
            ddog_ai_usage_known_metrics(profile.as_ptr(), profile.len(), &mut out),
            Status::Rejected
        );
        assert_eq!(json_of(out)["error"]["code"], "profile_invalid");
    }
}

#[test]
fn provider_ids_without_the_provider() {
    let call = |input: Value| {
        let input = input.to_string();
        let mut out = empty();
        // SAFETY: valid input and output.
        let status = unsafe { ddog_ai_usage_provider_ids(input.as_ptr(), input.len(), &mut out) };
        (status, json_of(out))
    };
    assert_eq!(
        call(json!({"headers": {"OpenAI-Organization": "acme", "openai-project": "proj_1"}})),
        (
            Status::Ok,
            json!({"trajectory.request.provider_account_id": "acme",
                "trajectory.request.project_id": "proj_1"})
        )
    );
    assert_eq!(
        call(json!({"provider_name": "",
            "models": ["arn:aws:bedrock:us-east-1:123456789012:inference-profile/x"]})),
        (
            Status::Ok,
            json!({"trajectory.request.provider_account_id": "123456789012"})
        )
    );
}

#[test]
fn stream_events_split_by_the_caller() {
    let mut stream = ptr::null_mut();
    let format = "openai_chat";
    // SAFETY: `stream` is written by `stream_new` and used until dropped.
    unsafe {
        assert_eq!(
            ddog_ai_usage_stream_new(format.as_ptr(), format.len(), false, &mut stream),
            Status::Ok
        );
        let data = r#"{"model": "gpt-4o", "usage": {"prompt_tokens": 4, "completion_tokens": 2}}"#;
        assert_eq!(
            ddog_ai_usage_stream_feed_event(stream, ptr::null(), 0, data.as_ptr(), data.len()),
            Status::Ok
        );
        let done = "[DONE]";
        assert_eq!(
            ddog_ai_usage_stream_feed_event(stream, ptr::null(), 0, done.as_ptr(), done.len()),
            Status::Ok
        );
        let mut out = empty();
        assert_eq!(ddog_ai_usage_stream_finish(stream, &mut out), Status::Ok);
        let value = json_of(out);
        assert_eq!(value["outcome"], "completed");
        assert_eq!(value["response_model"], "gpt-4o");
        assert_eq!(value["usage"]["input_tokens"], 4);
        assert_eq!(
            ddog_ai_usage_stream_feed_event(ptr::null_mut(), ptr::null(), 0, ptr::null(), 0),
            Status::Internal
        );
        ddog_ai_usage_stream_drop(stream);
    }
}

#[test]
fn batch_settings_and_quiet_adds() {
    let attempt = r#"{"profile": "gen_ai.client.provider_attempt@0.1.0",
        "observation": {"operation_name": "chat", "provider_name": "openai",
        "duration_seconds": 0.5, "streaming": false, "input_tokens": 10,
        "input_basis": "includes_cache", "output_tokens": 2,
        "output_basis": "includes_reasoning", "observation_point": "gateway"}}"#;
    let set = |f: unsafe extern "C" fn(*const Batch, *const u8, usize, *mut Buffer) -> Status,
               batch: *const Batch,
               input: &str| {
        let mut out = empty();
        // SAFETY: a live batch, valid input and output.
        let status = unsafe { f(batch, input.as_ptr(), input.len(), &mut out) };
        (status, json_of(out))
    };
    let mut batch = ptr::null_mut();
    // SAFETY: `batch` is written by `batch_new` and used until dropped.
    unsafe {
        assert_eq!(ddog_ai_usage_batch_new(&mut batch), Status::Ok);
        let (status, value) = set(
            ddog_ai_usage_batch_set_deployment_attributes,
            batch,
            r#"{"nope": "x"}"#,
        );
        assert_eq!(status, Status::Rejected);
        assert_eq!(value["error"]["code"], "deployment_attribute_unknown");
        assert_eq!(
            set(
                ddog_ai_usage_batch_set_deployment_attributes,
                batch,
                r#"{"user.id": "ada@example.com"}"#
            ),
            (Status::Ok, json!({}))
        );
        assert_eq!(
            set(
                ddog_ai_usage_batch_set_metrics,
                batch,
                r#"["gen_ai.client.inference.usage.input_tokens"]"#
            ),
            (Status::Ok, json!({}))
        );
        assert_eq!(
            set(ddog_ai_usage_batch_set_metrics, batch, r#"{"a": 1}"#).0,
            Status::Rejected
        );
        // Added without output; a null `out` is allowed.
        assert_eq!(
            ddog_ai_usage_batch_add_quiet(batch, attempt.as_ptr(), attempt.len(), ptr::null_mut()),
            Status::Ok
        );
        let mut out = empty();
        assert_eq!(
            ddog_ai_usage_batch_add_quiet(batch, attempt.as_ptr(), attempt.len(), &mut out),
            Status::Ok
        );
        assert_eq!(take(out), "");
        let rejected = r#"{"profile": "nope@1"}"#;
        let mut out = empty();
        assert_eq!(
            ddog_ai_usage_batch_add_quiet(batch, rejected.as_ptr(), rejected.len(), &mut out),
            Status::Rejected
        );
        assert_eq!(json_of(out)["error"]["code"], "profile_invalid");
        assert_eq!(
            ddog_ai_usage_batch_add_quiet(
                batch,
                rejected.as_ptr(),
                rejected.len(),
                ptr::null_mut()
            ),
            Status::Rejected
        );
        // The batch kept only the metric set, with the attributes set once.
        let (_, lines) = text_of(|out| {
            ddog_ai_usage_batch_take_dogstatsd_text(
                batch,
                ptr::null(),
                0,
                UDS_MAX_PAYLOAD,
                ptr::null_mut(),
                out,
            )
        });
        let lines: Vec<&str> = lines.lines().collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("gen_ai.client.inference.usage.input_tokens:20|c|#"));
        assert!(lines[0].contains("user.id:ada_example.com"), "{}", lines[0]);
        // A document's own list and attributes win over the batch's.
        let own = attempt.replace(
            r#""observation_point": "gateway"}}"#,
            r#""observation_point": "gateway"},
            "metrics": ["gen_ai.client.inference.usage.output_tokens"]}"#,
        );
        let mut out = empty();
        ddog_ai_usage_batch_add(batch, own.as_ptr(), own.len(), &mut out);
        take(out);
        let (_, lines) = text_of(|out| {
            ddog_ai_usage_batch_take_dogstatsd_text(
                batch,
                ptr::null(),
                0,
                UDS_MAX_PAYLOAD,
                ptr::null_mut(),
                out,
            )
        });
        assert!(lines.starts_with("gen_ai.client.inference.usage.output_tokens:2|c|#"));
        // Clearing the settings keeps every metric again.
        assert_eq!(
            set(ddog_ai_usage_batch_set_metrics, batch, "null"),
            (Status::Ok, json!({}))
        );
        ddog_ai_usage_batch_add_quiet(batch, attempt.as_ptr(), attempt.len(), ptr::null_mut());
        assert!(ddog_ai_usage_batch_len(batch) > 1);
        assert_eq!(
            ddog_ai_usage_batch_add_quiet(
                ptr::null(),
                attempt.as_ptr(),
                attempt.len(),
                ptr::null_mut()
            ),
            Status::Internal
        );
        ddog_ai_usage_batch_drop(batch);
    }
}

#[test]
fn stream_take_reports_and_releases() {
    let format = "openai_chat";
    let chunk = b"data: {\"model\": \"gpt-4o\", \"usage\": {\"prompt_tokens\": 4, \"completion_tokens\": 1}}\n\ndata: [DONE]";
    let mut stream = ptr::null_mut();
    // SAFETY: `stream` is written by `stream_new` and released by `take`.
    unsafe {
        assert_eq!(
            ddog_ai_usage_stream_new(format.as_ptr(), format.len(), false, &mut stream),
            Status::Ok
        );
        ddog_ai_usage_stream_feed(stream, chunk.as_ptr(), chunk.len());
        let mut out = empty();
        assert_eq!(ddog_ai_usage_stream_take(stream, &mut out), Status::Ok);
        let value = json_of(out);
        assert_eq!(value["outcome"], "completed");
        assert_eq!(value["usage"]["input_tokens"], 4);
        let mut out = empty();
        assert_eq!(
            ddog_ai_usage_stream_take(ptr::null_mut(), &mut out),
            Status::Internal
        );
        take(out);
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
    let add = |batch: *const Batch, out: *mut Buffer| {
        // SAFETY: a live batch, valid input, and a null or writable output.
        unsafe { ddog_ai_usage_batch_add_request(batch, request.as_ptr(), request.len(), out) }
    };
    let mut batch = ptr::null_mut();
    // SAFETY: `batch` is written by `batch_new` and used until dropped.
    unsafe {
        assert_eq!(ddog_ai_usage_batch_new(&mut batch), Status::Ok);
        let mut out = empty();
        assert_eq!(add(batch, &mut out), Status::Ok);
        let value = json_of(out);
        assert_eq!(value["dropped"], false);
        assert_eq!(value["issues"].as_array().unwrap().len(), 2);
        let one = ddog_ai_usage_batch_len(batch);
        let mut out = empty();
        ddog_ai_usage_batch_take_dogstatsd_text(
            batch,
            ptr::null(),
            0,
            1432,
            ptr::null_mut(),
            &mut out,
        );
        take(out);

        // Room for the attempt alone: neither it nor its breakdown is kept.
        ddog_ai_usage_batch_set_max_len(batch, one - 1);
        let mut out = empty();
        assert_eq!(add(batch, &mut out), Status::Ok);
        assert_eq!(json_of(out)["dropped"], true);
        assert!(ddog_ai_usage_batch_is_empty(batch));
        assert_eq!(ddog_ai_usage_batch_dropped(batch), 2);
        // Quietly, with a null output.
        ddog_ai_usage_batch_set_max_len(batch, 0);
        assert_eq!(add(batch, ptr::null_mut()), Status::Ok);
        assert_eq!(ddog_ai_usage_batch_len(batch), one);

        // A rejected document rejects the request and adds nothing.
        let bad = format!(r#"[{ATTEMPT}, {{"profile": "nope@1"}}]"#);
        let mut out = empty();
        let status = ddog_ai_usage_batch_add_request(batch, bad.as_ptr(), bad.len(), &mut out);
        assert_eq!(status, Status::Rejected);
        let error = json_of(out);
        assert_eq!(error["error"]["code"], "profile_invalid");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("document 1:")
        );
        assert_eq!(ddog_ai_usage_batch_len(batch), one);
        // A single document is not a request.
        let mut out = empty();
        let status =
            ddog_ai_usage_batch_add_request(batch, ATTEMPT.as_ptr(), ATTEMPT.len(), &mut out);
        assert_eq!(status, Status::Rejected);
        take(out);
        ddog_ai_usage_batch_drop(batch);
    }
}

/// The DogStatsD lines `batch` holds, taken.
///
/// # Safety
///
/// `batch` must be a live batch.
unsafe fn taken_lines(batch: *const Batch) -> String {
    let mut out = empty();
    // SAFETY: forwarded from the caller.
    unsafe {
        ddog_ai_usage_batch_take_dogstatsd_text(
            batch,
            ptr::null(),
            0,
            8192,
            ptr::null_mut(),
            &mut out,
        )
    };
    take(out)
}

/// Set a batch setting from JSON text.
///
/// # Safety
///
/// `batch` must be a live batch.
unsafe fn apply(
    setter: unsafe extern "C" fn(*const Batch, *const u8, usize, *mut Buffer) -> Status,
    batch: *const Batch,
    input: &str,
) -> Status {
    let mut out = empty();
    // SAFETY: forwarded from the caller.
    let status = unsafe { setter(batch, input.as_ptr(), input.len(), &mut out) };
    take(out);
    status
}

#[test]
fn batch_deployment_attributes_merge_with_the_document() {
    let bare = r#"{"profile": "gen_ai.client.provider_attempt@0.1.0",
        "observation": {"operation_name": "chat", "provider_name": "openai",
            "duration_seconds": 0.5, "streaming": false, "input_tokens": 10, "input_basis": "includes_cache",
            "output_tokens": 2, "output_basis": "includes_reasoning", "observation_point": "gateway"}"#;
    let with = |attributes: &str| format!("{bare}, \"deployment_attributes\": {attributes}}}");
    let mut batch = ptr::null_mut();
    // SAFETY: `batch` is written by `batch_new` and used until dropped.
    unsafe {
        assert_eq!(ddog_ai_usage_batch_new(&mut batch), Status::Ok);
        let defaults = r#"{"trajectory.gateway.route": "chat", "trajectory.team.id": "ml"}"#;
        assert_eq!(
            apply(
                ddog_ai_usage_batch_set_deployment_attributes,
                batch,
                defaults
            ),
            Status::Ok
        );
        assert_eq!(
            apply(
                ddog_ai_usage_batch_set_metrics,
                batch,
                r#"["gen_ai.client.inference.usage.input_tokens"]"#
            ),
            Status::Ok
        );
        // The document adds a user and overrides the team; the route stays.
        let document = with(r#"{"user.id": "ada@example.com", "trajectory.team.id": "infra"}"#);
        ddog_ai_usage_batch_add_quiet(batch, document.as_ptr(), document.len(), ptr::null_mut());
        let lines = taken_lines(batch);
        assert!(lines.contains("trajectory.gateway.route:chat"), "{lines}");
        assert!(lines.contains("trajectory.team.id:infra"), "{lines}");
        assert!(!lines.contains("trajectory.team.id:ml"), "{lines}");
        assert!(lines.contains("user.id:ada_example.com"), "{lines}");
        // `null` removes the batch's attribute for this document only.
        let document = with(r#"{"trajectory.gateway.route": null}"#);
        ddog_ai_usage_batch_add_quiet(batch, document.as_ptr(), document.len(), ptr::null_mut());
        let lines = taken_lines(batch);
        assert!(!lines.contains("trajectory.gateway.route"), "{lines}");
        assert!(lines.contains("trajectory.team.id:ml"), "{lines}");
        let document = format!("{bare}}}");
        ddog_ai_usage_batch_add_quiet(batch, document.as_ptr(), document.len(), ptr::null_mut());
        let lines = taken_lines(batch);
        assert!(lines.contains("trajectory.gateway.route:chat"), "{lines}");

        // `[]` keeps no metric; `null`, empty input or the reset call keep
        // every metric again.
        assert_eq!(
            apply(ddog_ai_usage_batch_set_metrics, batch, "[]"),
            Status::Ok
        );
        ddog_ai_usage_batch_add_quiet(batch, document.as_ptr(), document.len(), ptr::null_mut());
        assert!(ddog_ai_usage_batch_is_empty(batch));
        ddog_ai_usage_batch_reset_metrics(batch);
        ddog_ai_usage_batch_add_quiet(batch, document.as_ptr(), document.len(), ptr::null_mut());
        assert!(ddog_ai_usage_batch_len(batch) > 1);
        ddog_ai_usage_batch_reset_metrics(ptr::null());
        ddog_ai_usage_batch_drop(batch);
    }
}

/// A text call's status and output.
fn text_out(call: impl FnOnce(*mut Buffer) -> Status) -> (Status, String) {
    let mut out = empty();
    let status = call(&mut out);
    (status, take(out))
}

#[test]
fn operation_names_errors_and_costs_cross_the_boundary() {
    // SAFETY: valid inputs and outputs.
    unsafe {
        let route = "/v1/chat/completions?x=1";
        assert_eq!(
            text_out(|out| ddog_ai_usage_operation_name(route.as_ptr(), route.len(), out)),
            (Status::Ok, "chat".to_string())
        );
        let route = "/v1/models";
        assert_eq!(
            text_out(|out| ddog_ai_usage_operation_name(route.as_ptr(), route.len(), out)).0,
            Status::Rejected
        );
        assert_eq!(
            text_out(|out| ddog_ai_usage_error_type_for_failure(
                504,
                true,
                false,
                ptr::null(),
                0,
                out
            )),
            (Status::Ok, "timeout_error".to_string())
        );
        assert_eq!(
            text_out(|out| ddog_ai_usage_error_type_for_failure(
                0,
                true,
                false,
                ptr::null(),
                0,
                out
            )),
            (Status::Ok, "api_connection_error".to_string())
        );
        assert_eq!(
            text_out(|out| ddog_ai_usage_error_type_for_failure(
                200,
                false,
                false,
                ptr::null(),
                0,
                out
            ))
            .0,
            Status::Rejected
        );
        let mut cost = 0;
        assert!(ddog_ai_usage_estimated_cost(
            3.0, 15.0, 0.3, -1.0, 1_000_000, 400_000, 100_000, 10_000, &mut cost
        ));
        // 0.5M at 3 + 0.4M at 0.3 + 0.1M at 3 (as input) + 0.01M at 15.
        assert_eq!(cost, 2_070_000_000);
        assert!(!ddog_ai_usage_estimated_cost(
            f64::NAN,
            15.0,
            -1.0,
            -1.0,
            1,
            0,
            0,
            1,
            &mut cost
        ));
    }
}

#[test]
fn extended_observations_high_water_and_drops_since_last_take() {
    let document = r#"{"profile": "gen_ai.client.provider_attempt@0.1.0",
        "observation": {"operation_name": "chat", "provider_name": "openai",
            "duration_ns": 500000000, "streaming": false, "input_tokens": 1000,
            "input_basis": "includes_cache", "output_tokens": 100,
            "output_basis": "includes_reasoning", "observation_point": "gateway",
            "prices_per_million": {"input": 2.5, "output": 10}}}"#;
    let mut batch = ptr::null_mut();
    // SAFETY: `batch` is written by `batch_new` and used until dropped.
    unsafe {
        assert_eq!(ddog_ai_usage_batch_new(&mut batch), Status::Ok);
        let mut out = empty();
        assert_eq!(
            ddog_ai_usage_batch_add(batch, document.as_ptr(), document.len(), &mut out),
            Status::Ok
        );
        assert_eq!(json_of(out)["full"], false);
        let one = ddog_ai_usage_batch_len(batch);
        ddog_ai_usage_batch_set_high_water(batch, one + 1);
        let mut out = empty();
        ddog_ai_usage_batch_add(batch, document.as_ptr(), document.len(), &mut out);
        assert_eq!(json_of(out)["full"], true);
        assert!(ddog_ai_usage_batch_is_full(batch));
        let mut out = empty();
        ddog_ai_usage_batch_take_dogstatsd_text(
            batch,
            ptr::null(),
            0,
            8192,
            ptr::null_mut(),
            &mut out,
        );
        let lines = take(out);
        // (1000 at 2.5 + 100 at 10) per million = 0.0035 USD, twice.
        assert!(
            lines.contains("trajectory.gen_ai.client.inference.usage.cost:0.007|c|#"),
            "{lines}"
        );
        assert!(
            lines.contains("trajectory.cost.source:estimated"),
            "{lines}"
        );
        assert!(
            lines.contains("gen_ai.client.inference.duration:0.5|d|#"),
            "{lines}"
        );
        assert!(!ddog_ai_usage_batch_is_full(batch));
        // Drops since the last take.
        ddog_ai_usage_batch_set_max_len(batch, 1);
        for _ in 0..3 {
            ddog_ai_usage_batch_add_quiet(
                batch,
                document.as_ptr(),
                document.len(),
                ptr::null_mut(),
            );
        }
        assert_eq!(ddog_ai_usage_batch_take_dropped(batch), 3);
        assert_eq!(ddog_ai_usage_batch_take_dropped(batch), 0);
        assert_eq!(ddog_ai_usage_batch_dropped(batch), 3);
        ddog_ai_usage_batch_set_high_water(ptr::null(), 1);
        assert!(!ddog_ai_usage_batch_is_full(ptr::null()));
        assert_eq!(ddog_ai_usage_batch_take_dropped(ptr::null()), 0);
        ddog_ai_usage_batch_drop(batch);
    }
}

#[test]
fn otel_metrics_translate_across_the_boundary() {
    let translate = |input: &str| {
        let mut out = empty();
        // SAFETY: valid input and output.
        let status =
            unsafe { ddog_ai_usage_translate_otel_metric(input.as_ptr(), input.len(), &mut out) };
        (status, json_of(out))
    };
    let (status, value) = translate(
        r#"{"name": "gen_ai_client_token_usage_sum",
            "attributes": {"gen_ai_token_type": "input", "gen_ai_system": "amazon-bedrock"}}"#,
    );
    assert_eq!(status, Status::Ok);
    assert_eq!(
        value,
        json!({
            "name": "gen_ai.client.inference.usage.input_tokens",
            "profile": "gen_ai.client.provider_attempt@0.1.0",
            "attributes": {"gen_ai.provider.name": "aws.bedrock"},
        })
    );
    let (status, value) = translate(r#"{"name": "process.cpu.time"}"#);
    assert_eq!(status, Status::Rejected);
    assert_eq!(value["error"]["code"], "value_not_allowed");
    let (status, _) = translate(r#"{"name": 1}"#);
    assert_eq!(status, Status::Rejected);
}

#[test]
fn batch_two_entry_points() {
    // SAFETY: valid inputs and outputs throughout.
    unsafe {
        let id = " acct-1 ";
        assert_eq!(
            text_out(|out| ddog_ai_usage_billing_id(id.as_ptr(), id.len(), out)),
            (Status::Ok, "acct-1".to_string())
        );
        let long = "a".repeat(129);
        assert_eq!(
            text_out(|out| ddog_ai_usage_billing_id(long.as_ptr(), long.len(), out)).0,
            Status::Rejected
        );
        assert_eq!(
            text_out(|out| ddog_ai_usage_error_type_failed(ptr::null(), 0, 0, out)),
            (Status::Ok, "api_error".to_string())
        );
        assert_eq!(
            text_out(|out| ddog_ai_usage_error_type_failed(ptr::null(), 0, 503, out)).1,
            "internal_server_error"
        );
        let url = "https://generativelanguage.googleapis.com/v1beta/openai/";
        let provider = "openai";
        assert_eq!(
            text_out(|out| ddog_ai_usage_usage_format_for_base_url(
                url.as_ptr(),
                url.len(),
                provider.as_ptr(),
                provider.len(),
                out
            )),
            (Status::Ok, "gemini_openai_compat".to_string())
        );
        let datagram = "a:1|c\nb:1|c\nc:1|c";
        let mut dropped = 9;
        assert_eq!(
            text_out(|out| ddog_ai_usage_repack_dogstatsd(
                datagram.as_ptr(),
                datagram.len(),
                11,
                &mut dropped,
                out
            )),
            (Status::Ok, "a:1|c\nb:1|c\0c:1|c".to_string()),
        );
        assert_eq!(dropped, 0);
        let mut rejected = 0;
        let body = br#"{"partialSuccess": {"rejectedDataPoints": "4", "errorMessage": "late"}}"#;
        assert_eq!(
            text_out(|out| ddog_ai_usage_otlp_partial_success(
                body.as_ptr(),
                body.len(),
                &mut rejected,
                out
            )),
            (Status::Ok, "late".to_string())
        );
        assert_eq!(rejected, 4);
        assert_eq!(
            text_out(|out| ddog_ai_usage_otlp_partial_success(ptr::null(), 0, &mut rejected, out))
                .0,
            Status::Rejected
        );
    }
}

#[test]
fn otlp_parts_are_framed_by_length() {
    let mut batch = ptr::null_mut();
    let scope = "litellm";
    let version = "1.0";
    // SAFETY: `batch` is written by `batch_new` and used until dropped.
    unsafe {
        assert_eq!(ddog_ai_usage_batch_new(&mut batch), Status::Ok);
        for user in 0..40 {
            let document = ATTEMPT.replace("ada@example.com", &format!("user{user}@example.com"));
            ddog_ai_usage_batch_add_quiet(
                batch,
                document.as_ptr(),
                document.len(),
                ptr::null_mut(),
            );
        }
        let mut out = empty();
        let status = ddog_ai_usage_batch_take_otlp_parts(
            batch,
            scope.as_ptr(),
            scope.len(),
            version.as_ptr(),
            version.len(),
            1,
            2,
            false,
            2048,
            &mut out,
        );
        assert_eq!(status, Status::Ok);
        let framed = std::slice::from_raw_parts(out.ptr, out.len).to_vec();
        ddog_ai_usage_buffer_drop(&mut out);
        let mut parts = 0;
        let mut at = 0;
        while at < framed.len() {
            let len = u32::from_le_bytes(framed[at..at + 4].try_into().unwrap()) as usize;
            at += 4 + len;
            assert!(len > 0);
            parts += 1;
        }
        assert_eq!(at, framed.len());
        assert!(parts > 1, "{parts}");
        assert!(ddog_ai_usage_batch_is_empty(batch));
        // An empty batch writes nothing.
        let mut out = empty();
        let status = ddog_ai_usage_batch_take_otlp_parts(
            batch,
            scope.as_ptr(),
            scope.len(),
            version.as_ptr(),
            version.len(),
            1,
            2,
            true,
            0,
            &mut out,
        );
        assert_eq!((status, out.len), (Status::Ok, 0));
        ddog_ai_usage_batch_drop(batch);
    }
}

#[test]
fn a_batch_taken_as_intake_series() {
    let request = format!("[{ATTEMPT}, {BREAKDOWN}]");
    let host = "gateway-7f9";
    let mut batch = ptr::null_mut();
    // SAFETY: `batch` is written by `batch_new` and used until dropped.
    unsafe {
        assert_eq!(ddog_ai_usage_batch_new(&mut batch), Status::Ok);
        ddog_ai_usage_batch_set_writer(batch, host.as_ptr(), host.len(), 7);
        for _ in 0..3 {
            assert_eq!(
                ddog_ai_usage_batch_add_request(
                    batch,
                    request.as_ptr(),
                    request.len(),
                    ptr::null_mut()
                ),
                Status::Ok
            );
        }
        let mut out = empty();
        assert_eq!(
            ddog_ai_usage_batch_take_series(batch, 10, 1_700_000_000, &mut out),
            Status::Ok
        );
        let value = json_of(out);
        assert_eq!(value["timestamp"], 1_700_000_000);
        assert_eq!(value["interval"], 10);
        assert_eq!(value["dropped"], 0);
        let series = value["series"].as_array().unwrap();
        let input = series
            .iter()
            .find(|s| s["metric"] == "gen_ai.client.inference.usage.input_tokens")
            .unwrap();
        // Three attempts of 10 input tokens over 10 s.
        assert_eq!(input["value"], 3.0);
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
        let mut out = empty();
        ddog_ai_usage_batch_take_series(batch, 10, 1_700_000_000, &mut out);
        let value = json_of(out);
        assert_eq!(value["timestamp"], 1_700_000_001);
        assert_eq!(value["series"], json!([]));
        ddog_ai_usage_batch_set_writer(ptr::null(), host.as_ptr(), host.len(), 1);
        ddog_ai_usage_batch_drop(batch);
    }
}
