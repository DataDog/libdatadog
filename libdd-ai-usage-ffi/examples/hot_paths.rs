// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Time and allocations of the calls a gateway makes per request, through
//! the C interface: `cargo run --release -p libdd-ai-usage-ffi --example
//! hot_paths`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use dd_ai_usage_ffi::*;

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(by: usize) {
    let live = LIVE.fetch_add(by, Ordering::Relaxed) + by;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

// SAFETY: forwards to the system allocator, counting calls and live bytes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        grow(layout.size());
        // SAFETY: forwarded.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: forwarded.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        grow(size);
        // SAFETY: forwarded.
        unsafe { System.realloc(ptr, layout, size) }
    }
}

/// The time of one call in milliseconds (best of 5) and the most memory
/// it held at once beyond what was live before it.
fn measure_burst(label: &str, mut call: impl FnMut()) {
    let mut best = f64::MAX;
    let mut peak = 0;
    for _ in 0..5 {
        let base = LIVE.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        let start = Instant::now();
        call();
        best = best.min(start.elapsed().as_secs_f64() * 1e3);
        peak = peak.max(PEAK.load(Ordering::Relaxed) - base);
    }
    println!(
        "{label:<44} {best:>8.2} ms {:>8.1} MiB peak",
        peak as f64 / (1024.0 * 1024.0)
    );
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// The best time of a call, in microseconds, and its allocations.
fn measure(label: &str, mut call: impl FnMut()) {
    call();
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    call();
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    let mut best = f64::MAX;
    for _ in 0..20 {
        let start = Instant::now();
        for _ in 0..200 {
            call();
        }
        best = best.min(start.elapsed().as_secs_f64() * 1e6 / 200.0);
    }
    println!("{label:<44} {best:>8.2} us {allocations:>6} allocations");
}

fn drop_out(out: &mut Buffer) {
    // SAFETY: written by the library.
    unsafe { ddog_ai_usage_buffer_drop(out) };
}

const ATTEMPT: &str = r#"{"profile": "gen_ai.client.provider_attempt@0.1.0",
  "observation": {"operation_name": "chat", "provider_name": "openai",
    "request_model": "gpt-4o", "response_model": "gpt-4o-2024-08-06",
    "duration_seconds": 1.2, "streaming": true, "time_to_first_chunk_seconds": 0.3,
    "input_tokens": 1000, "input_basis": "includes_cache", "output_tokens": 500,
    "output_basis": "includes_reasoning", "service_tier": "default",
    "cost_nano_usd": 11500000, "cost_source": "calculated", "cost_rate_card": "openai@1",
    "observation_point": "gateway"},
  "deployment_attributes": {"user.id": "ada@example.com"}}"#;

const BREAKDOWN: &str = r#"{"profile": "trajectory.gen_ai.client.token_breakdown@0.1.0",
  "observation": {"operation_name": "chat", "provider_name": "anthropic",
    "request_model": "claude-sonnet-4-5", "input_tokens": 100, "input_basis": "excludes_cache",
    "output_tokens": 50, "output_basis": "includes_reasoning", "cache_read_input_tokens": 400,
    "cache_write_input_tokens": 20, "reasoning_output_tokens": 10,
    "observation_point": "gateway"}}"#;

const GATEWAY: &str = r#"{"profile": "trajectory.gen_ai.gateway.request@0.1.0",
  "observation": {"operation_name": "chat", "request_model": "gpt-4o",
    "duration_seconds": 1.4, "provider_operations": 1,
    "provider_operation_coverage": "complete", "cache_outcomes": ["miss"]}}"#;

/// A small Responses `response.completed` event, as OpenAI sends it.
const RESPONSE_COMPLETED: &str = r#"{"type":"response.completed","sequence_number":20,"response":{"id":"resp_68af","object":"response","created_at":1756300000,"status":"completed","background":false,"error":null,"incomplete_details":null,"instructions":null,"max_output_tokens":null,"max_tool_calls":null,"model":"gpt-4.1-2025-04-14","output":[{"id":"msg_68af","type":"message","status":"completed","content":[{"type":"output_text","annotations":[],"logprobs":[],"text":"Hello! How can I help you today? Let me know what you would like to work on."}],"role":"assistant"}],"parallel_tool_calls":true,"previous_response_id":null,"prompt_cache_key":null,"reasoning":{"effort":null,"summary":null},"safety_identifier":null,"service_tier":"default","store":true,"temperature":1.0,"text":{"format":{"type":"text"},"verbosity":"medium"},"tool_choice":"auto","tools":[],"top_logprobs":0,"top_p":1.0,"truncation":"disabled","usage":{"input_tokens":12,"input_tokens_details":{"cached_tokens":0},"output_tokens":18,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":30},"user":null,"metadata":{}}}"#;

/// A Chat Completions body of about `size` bytes: a long answer with line
/// breaks and quotes, and a tool call whose arguments are escaped JSON.
fn chat_body(size: usize) -> String {
    let sentence = r#"The quick brown fox said \"hello\" and jumped over the lazy dog.\n"#;
    let args = r#"{\"path\": \"src/main.rs\", \"line\": 42, \"text\": \"fn main() {}\"}"#;
    let mut content = String::with_capacity(size);
    while content.len() < size * 3 / 4 {
        content.push_str(sentence);
    }
    let mut arguments = String::with_capacity(size / 4);
    while arguments.len() < size / 4 {
        arguments.push_str(args);
    }
    format!(
        r#"{{"id": "chatcmpl-1", "object": "chat.completion", "created": 1, "model": "gpt-4o-2024-08-06", "choices": [{{"index": 0, "message": {{"role": "assistant", "content": "{content}", "tool_calls": [{{"id": "call_1", "type": "function", "function": {{"name": "edit", "arguments": "{arguments}"}}}}]}}, "logprobs": null, "finish_reason": "stop"}}], "usage": {{"prompt_tokens": 1200, "completion_tokens": 9000, "total_tokens": 10200, "prompt_tokens_details": {{"cached_tokens": 0}}, "completion_tokens_details": {{"reasoning_tokens": 0}}}}, "service_tier": "default", "system_fingerprint": "fp_1"}}"#
    )
}

fn main() {
    // 50,000 DogStatsD lines packed into UDS datagrams, caller's encoding
    // and decoding included, as a gateway in another language pays them.
    let lines: Vec<String> = (0..50_000)
        .map(|i| {
            format!(
                "gen_ai.client.inference.usage.input_tokens:{i}|c|#gen_ai.operation.name:chat,\
                 gen_ai.provider.name:openai,gen_ai.request.model:gpt-4o,\
                 gen_ai.token.modality:unknown,trajectory.observation.point:gateway,\
                 trajectory.profile:gen_ai.client.provider_attempt/0.1.0"
            )
        })
        .collect();
    measure_burst("pack 50k lines, JSON path", || {
        let request = serde_json::json!({
            "lines": lines,
            "constant_tags": ["env:prod", "version:1.2"],
            "max_payload": UDS_MAX_PAYLOAD,
        })
        .to_string();
        let mut out = Buffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        // SAFETY: valid input and output.
        unsafe { ddog_ai_usage_pack_dogstatsd(request.as_ptr(), request.len(), &mut out) };
        // SAFETY: written by the library.
        let text = unsafe { std::slice::from_raw_parts(out.ptr, out.len) };
        let value: serde_json::Value = serde_json::from_slice(text).unwrap();
        assert!(value["datagrams"].as_array().unwrap().len() > 100);
        drop(value);
        drop_out(&mut out);
    });
    measure_burst("pack 50k lines, text path", || {
        let request = lines.join("\n");
        let tags = "env:prod,version:1.2";
        let mut out = Buffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        // SAFETY: valid inputs and output.
        unsafe {
            ddog_ai_usage_pack_dogstatsd_lines(
                request.as_ptr(),
                request.len(),
                tags.as_ptr(),
                tags.len(),
                UDS_MAX_PAYLOAD,
                ptr::null_mut(),
                &mut out,
            )
        };
        // SAFETY: written by the library.
        let text = unsafe { std::slice::from_raw_parts(out.ptr, out.len) };
        let datagrams: Vec<&[u8]> = text.split(|b| *b == 0).collect();
        assert!(datagrams.len() > 100);
        drop(datagrams);
        drop_out(&mut out);
    });
    // 2,000 attempts of distinct users taken as OTLP: one request, and
    // requests of at most 64 KiB. Both fill the batch the same way, so the
    // difference is the cost of splitting.
    let users: Vec<String> = (0..2_000)
        .map(|user| ATTEMPT.replace("ada@example.com", &format!("user{user}@example.com")))
        .collect();
    for (label, max_bytes) in [
        ("take_otlp_parts 2k attempts, one request", 0),
        ("take_otlp_parts 2k attempts, 64 KiB parts", 64 * 1024),
    ] {
        measure_burst(label, || {
            let mut batch = ptr::null_mut();
            let mut out = Buffer {
                ptr: ptr::null_mut(),
                len: 0,
            };
            let scope = "bench";
            // SAFETY: a live batch, valid inputs and output.
            unsafe {
                ddog_ai_usage_batch_new(&mut batch);
                for document in &users {
                    ddog_ai_usage_batch_add_quiet(
                        batch,
                        document.as_ptr(),
                        document.len(),
                        ptr::null_mut(),
                    );
                }
                let status = ddog_ai_usage_batch_take_otlp_parts(
                    batch,
                    scope.as_ptr(),
                    scope.len(),
                    scope.as_ptr(),
                    scope.len(),
                    1,
                    2,
                    false,
                    max_bytes,
                    &mut out,
                );
                assert_eq!(status, Status::Ok);
                ddog_ai_usage_batch_drop(batch);
            }
            drop_out(&mut out);
        });
    }
    measure("dogstatsd_lines provider_attempt (text)", || {
        let mut out = Buffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        // SAFETY: valid input and output.
        unsafe { ddog_ai_usage_dogstatsd_lines(ATTEMPT.as_ptr(), ATTEMPT.len(), &mut out) };
        drop_out(&mut out);
    });

    for (label, doc) in [
        ("project provider_attempt", ATTEMPT),
        ("project token_breakdown", BREAKDOWN),
        ("project gateway_request", GATEWAY),
    ] {
        let mut out = Buffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        // SAFETY: valid input and output.
        let status = unsafe { ddog_ai_usage_project(doc.as_ptr(), doc.len(), &mut out) };
        assert_eq!(status, Status::Ok, "{label}");
        drop_out(&mut out);
        measure(label, || {
            // SAFETY: valid input and output.
            unsafe { ddog_ai_usage_project(doc.as_ptr(), doc.len(), &mut out) };
            drop_out(&mut out);
        });
    }

    let mut batch = ptr::null_mut();
    // SAFETY: written by `batch_new`.
    unsafe { ddog_ai_usage_batch_new(&mut batch) };
    for (label, doc) in [
        ("batch_add provider_attempt", ATTEMPT),
        ("batch_add token_breakdown", BREAKDOWN),
    ] {
        measure(label, || {
            let mut out = Buffer {
                ptr: ptr::null_mut(),
                len: 0,
            };
            // SAFETY: a live batch, valid input and output.
            unsafe { ddog_ai_usage_batch_add(batch, doc.as_ptr(), doc.len(), &mut out) };
            drop_out(&mut out);
        });
    }
    // SAFETY: a live batch, released once.
    unsafe { ddog_ai_usage_batch_drop(batch) };

    // The same documents with the deployment attributes set once on the
    // batch, added without output.
    let mut batch = ptr::null_mut();
    let fixed = r#"{"user.id": "ada@example.com"}"#;
    // SAFETY: written by `batch_new`; valid inputs and outputs.
    unsafe {
        ddog_ai_usage_batch_new(&mut batch);
        let mut out = Buffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        let status = ddog_ai_usage_batch_set_deployment_attributes(
            batch,
            fixed.as_ptr(),
            fixed.len(),
            &mut out,
        );
        assert_eq!(status, Status::Ok);
        drop_out(&mut out);
    }
    let attempt = ATTEMPT.replace(
        ",\n  \"deployment_attributes\": {\"user.id\": \"ada@example.com\"}",
        "",
    );
    assert_ne!(attempt, ATTEMPT);
    for (label, doc) in [
        ("batch_add_quiet provider_attempt", attempt.as_str()),
        ("batch_add_quiet token_breakdown", BREAKDOWN),
    ] {
        measure(label, || {
            // SAFETY: a live batch and valid input.
            let status = unsafe {
                ddog_ai_usage_batch_add_quiet(batch, doc.as_ptr(), doc.len(), ptr::null_mut())
            };
            assert_eq!(status, Status::Ok);
        });
    }
    // The same attempt stating what a gateway derives (Kong): integer
    // nanoseconds, per-million prices, and zeros that mean "not reported".
    let extended = attempt
        .replace("\"duration_seconds\": 1.2,", "\"duration_ns\": 1200000000,")
        .replace(
            "\"cost_nano_usd\": 11500000, \"cost_source\": \"calculated\", \"cost_rate_card\": \"openai@1\",",
            "\"prices_per_million\": {\"input\": 2.5, \"output\": 10}, \"zero_is_unreported\": true,",
        );
    assert_ne!(extended, attempt);
    measure("batch_add_quiet provider_attempt, extended", || {
        // SAFETY: a live batch and valid input.
        let status = unsafe {
            ddog_ai_usage_batch_add_quiet(batch, extended.as_ptr(), extended.len(), ptr::null_mut())
        };
        assert_eq!(status, Status::Ok);
    });
    // SAFETY: a live batch, released once.
    unsafe { ddog_ai_usage_batch_drop(batch) };

    let body = r#"{"id": "chatcmpl-1", "object": "chat.completion", "model": "gpt-4o-2024-08-06",
      "service_tier": "default", "choices": [{"index": 0, "message": {"role": "assistant",
      "content": "Hello! How can I help you today?"}, "finish_reason": "stop"}],
      "usage": {"prompt_tokens": 19, "completion_tokens": 10, "total_tokens": 29,
      "prompt_tokens_details": {"cached_tokens": 0}, "completion_tokens_details":
      {"reasoning_tokens": 0}}}"#;
    let format = "openai_chat";
    measure("parse_provider_response openai_chat", || {
        let mut out = Buffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        // SAFETY: valid inputs and output.
        unsafe {
            ddog_ai_usage_parse_provider_response(
                format.as_ptr(),
                format.len(),
                false,
                body.as_ptr(),
                body.len(),
                &mut out,
            )
        };
        drop_out(&mut out);
    });

    // Chat Completions bodies of 50 and 500 KB, mostly generated text with
    // the escapes prose and tool arguments carry.
    for kb in [50, 500] {
        let body = chat_body(kb * 1024);
        measure(
            &format!("parse_provider_response openai_chat {kb} KB"),
            || {
                let mut out = Buffer {
                    ptr: ptr::null_mut(),
                    len: 0,
                };
                // SAFETY: valid inputs and output.
                unsafe {
                    ddog_ai_usage_parse_provider_response(
                        format.as_ptr(),
                        format.len(),
                        false,
                        body.as_ptr(),
                        body.len(),
                        &mut out,
                    )
                };
                drop_out(&mut out);
            },
        );
    }

    // A small Responses `response.completed` event, fed as one line.
    let responses = "openai_responses";
    let completed = format!("event: response.completed\ndata: {RESPONSE_COMPLETED}\n\n");
    measure("stream response.completed: new + feed + take", || {
        let mut stream = ptr::null_mut();
        let mut out = Buffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        // SAFETY: the stream is used until taken; valid inputs and output.
        unsafe {
            ddog_ai_usage_stream_new(responses.as_ptr(), responses.len(), false, &mut stream);
            ddog_ai_usage_stream_feed(stream, completed.as_ptr(), completed.len());
            ddog_ai_usage_stream_take(stream, &mut out);
        }
        drop_out(&mut out);
    });

    // An OpenAI chat stream with usage requested: every chunk names the
    // model and tier and carries `"usage": null`; the last one has usage.
    let mut chunks: Vec<String> = (0..32)
        .map(|i| {
            format!(
                "data: {{\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\
                 \"model\":\"gpt-4o-2024-08-06\",\"service_tier\":\"default\",\
                 \"system_fingerprint\":\"fp_1\",\"choices\":[{{\"index\":0,\"delta\":\
                 {{\"content\":\"word{i} \"}},\"logprobs\":null,\"finish_reason\":null}}],\
                 \"usage\":null}}\n\n"
            )
        })
        .collect();
    chunks.push(
        "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1,\
         \"model\":\"gpt-4o-2024-08-06\",\"service_tier\":\"default\",\"choices\":[],\
         \"usage\":{\"prompt_tokens\":19,\"completion_tokens\":32,\"total_tokens\":51}}\n\n"
            .to_string(),
    );
    chunks.push("data: [DONE]\n\n".to_string());
    measure("stream 34 chunks: new + feed + finish + drop", || {
        let mut stream = ptr::null_mut();
        let mut out = Buffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        // SAFETY: the stream is used until dropped; valid inputs and output.
        unsafe {
            ddog_ai_usage_stream_new(format.as_ptr(), format.len(), false, &mut stream);
            for chunk in &chunks {
                ddog_ai_usage_stream_feed(stream, chunk.as_ptr(), chunk.len());
            }
            ddog_ai_usage_stream_finish(stream, &mut out);
            ddog_ai_usage_stream_drop(stream);
        }
        drop_out(&mut out);
    });
    measure("stream 34 chunks: new + feed + take", || {
        let mut stream = ptr::null_mut();
        let mut out = Buffer {
            ptr: ptr::null_mut(),
            len: 0,
        };
        // SAFETY: the stream is used until taken; valid inputs and output.
        unsafe {
            ddog_ai_usage_stream_new(format.as_ptr(), format.len(), false, &mut stream);
            for chunk in &chunks {
                ddog_ai_usage_stream_feed(stream, chunk.as_ptr(), chunk.len());
            }
            ddog_ai_usage_stream_take(stream, &mut out);
        }
        drop_out(&mut out);
    });
}
