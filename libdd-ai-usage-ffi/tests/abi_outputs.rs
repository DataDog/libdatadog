// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Every output of the C interface, for the conformance cases and provider
//! fixtures of libdd-ai-usage, is byte-identical to what ABI v1 wrote for the
//! same input. `tests/data/abi-v1-outputs.json` holds the length and FNV-1a
//! digest of each v1 output, recorded with this corpus from ABI v1 before
//! the move to libdd-common-ffi types; it is not regenerated.

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::ptr;

use dd_ai_usage_ffi::*;
use libdd_common_ffi::slice::ByteSlice;
use libdd_common_ffi::{CharSlice, Handle, StringWrapper};
use serde_json::{Value, json};

/// The calls the corpus makes. Each returns `{status}:{output}`, where a
/// rejection's output is `{"error": {"code", "message"}}` (empty for a text
/// call that maps to nothing) as ABI v1 wrote it.
trait Api {
    fn project(&self, input: &str) -> String;
    /// Add `request` to a fresh batch, take it as intake, add it again and
    /// take it as DogStatsD text.
    fn batch(&self, request: &str) -> String;
    fn parse(&self, format: &str, embeddings: bool, options: u32, body: &[u8]) -> String;
    fn stream(&self, format: &str, embeddings: bool, options: u32, body: &[u8]) -> String;
    fn known_metrics(&self, profile: &str) -> String;
    fn provider_id_headers(&self, provider: &str) -> String;
    fn provider_ids(&self, input: &str) -> String;
    fn provider_name(&self, name: &str) -> String;
    fn operation_name(&self, route: &str) -> String;
    fn error_type(&self, name: &str, status: u16) -> String;
    fn error_type_known(&self, name: &str, status: u16) -> String;
    fn tag_value(&self, value: &str) -> String;
}

fn json_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            json_files(&path, files);
        } else if path.extension().is_some_and(|e| e == "json") {
            files.push(path);
        }
    }
}

fn core_tests() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../libdd-ai-usage/tests")
}

/// Every output of the corpus, by case name.
fn outputs(api: &dyn Api) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let root = core_tests();

    // Conformance cases: their request document projected and batched.
    let data = root.join("data");
    let mut files = Vec::new();
    json_files(&data, &mut files);
    files.sort();
    for path in &files {
        let text = fs::read_to_string(path).unwrap();
        // A case holding a number no double holds (`1e400`) is sent whole.
        let document = match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(case)) => {
                let mut document = serde_json::Map::new();
                for key in ["profile", "observation", "deployment_attributes", "metrics"] {
                    if let Some(value) = case.get(key) {
                        document.insert(key.to_string(), value.clone());
                    }
                }
                if document.is_empty() {
                    continue;
                }
                Value::Object(document).to_string()
            }
            Ok(_) => continue,
            Err(_) => text,
        };
        let name = path.strip_prefix(&data).unwrap().display().to_string();
        out.insert(format!("project/{name}"), api.project(&document));
        out.insert(format!("batch/{name}"), api.batch(&format!("[{document}]")));
    }

    // Provider fixtures: the body read whole and as a stream.
    let fixtures = root.join("fixtures/provider-usage");
    let mut files = Vec::new();
    json_files(&fixtures, &mut files);
    files.sort();
    for path in &files {
        let case: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let format = case["format"].as_str().unwrap();
        let embeddings = case["operation_name"] == "embeddings";
        let body = match case["body"].as_str() {
            Some(body) => body.to_string(),
            None => case["payload"].to_string(),
        };
        for options in [0, INCLUSIVE_TOTALS] {
            out.insert(
                format!("parse/{options}/{name}"),
                api.parse(format, embeddings, options, body.as_bytes()),
            );
            out.insert(
                format!("stream/{options}/{name}"),
                api.stream(format, embeddings, options, body.as_bytes()),
            );
        }
        out.insert(
            format!("parse/auto/{name}"),
            api.parse("", embeddings, 0, body.as_bytes()),
        );
    }

    for profile in [
        "",
        "gen_ai.client.provider_attempt@0.1.0",
        "trajectory.gen_ai.client.token_breakdown@0.1.0",
        "gen_ai.client.provider_streaming@0.1.0",
        "trajectory.gen_ai.gateway.request@0.1.0",
        "nope@1",
    ] {
        out.insert(
            format!("known_metrics/{profile}"),
            api.known_metrics(profile),
        );
    }
    for provider in [
        "openai",
        "anthropic",
        "aws.bedrock",
        "",
        "\"openai\"",
        "{\"a\": 1}",
    ] {
        out.insert(
            format!("provider_id_headers/{provider}"),
            api.provider_id_headers(provider),
        );
    }
    for (index, input) in [
        json!({"provider_name": "openai",
            "headers": {"OpenAI-Organization": "acme", "openai-project": "proj_1"}}),
        json!({"provider_name": "anthropic", "headers": {"anthropic-organization-id": "org-1"}}),
        json!({"provider_name": "aws.bedrock",
            "models": ["arn:aws:bedrock:us-east-1:123456789012:inference-profile/x"]}),
        json!({"upstream_url":
            "https://us-east5-aiplatform.googleapis.com/v1/projects/p-1/locations/x"}),
        json!({"headers": {"OpenAI-Organization": "acme"}}),
        json!([1, 2]),
    ]
    .iter()
    .enumerate()
    {
        out.insert(
            format!("provider_ids/{index}"),
            api.provider_ids(&input.to_string()),
        );
    }
    for name in [
        "openai",
        "bedrock",
        "vertex_ai",
        "Azure OpenAI",
        "gemini",
        "google",
        "1",
        "",
    ] {
        out.insert(format!("provider_name/{name}"), api.provider_name(name));
    }
    for route in [
        "/v1/chat/completions?x=1",
        "/v1/messages",
        "/v1/responses",
        "/v1/embeddings",
        "/v1/completions",
        "/v1beta/models/gemini-2.5-pro:generateContent",
        "/model/x/converse",
        "/v1/models",
        "chat",
        "",
    ] {
        out.insert(format!("operation_name/{route}"), api.operation_name(route));
    }
    for (name, status) in [
        ("RateLimitError", 0),
        ("", 503),
        ("", 200),
        ("", 0),
        ("SomethingElse", 0),
        ("api_error", 0),
        ("404", 500),
        ("RESOURCE_EXHAUSTED", 0),
        ("unknown", 0),
        ("timeout", 408),
    ] {
        out.insert(
            format!("error_type/{name}/{status}"),
            api.error_type(name, status),
        );
        out.insert(
            format!("error_type_known/{name}/{status}"),
            api.error_type_known(name, status),
        );
    }
    for value in [
        "Hello World",
        "a:b",
        "ÅÄÖ é",
        "  x  ",
        "",
        "user@example.com",
    ] {
        out.insert(format!("tag_value/{value}"), api.tag_value(value));
    }
    out
}

/// FNV-1a, 64 bits: a digest that is the same on every platform and release.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn digest(output: &str) -> String {
    format!("{}:{:016x}", output.len(), fnv1a(output.as_bytes()))
}

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/abi-v1-outputs.json")
}

// The calls, rendered as ABI v1 wrote them: v1 wrote a rejection's code and
// message into the output as `{"error": {"code", "message"}}`, and nothing
// for a text call that mapped to nothing; v2 returns them as an error.

fn cs(text: &str) -> CharSlice<'_> {
    CharSlice::from(text)
}

fn call(f: impl FnOnce(*mut StringWrapper) -> Option<Box<AiUsageError>>) -> String {
    let mut out = StringWrapper::from(String::new());
    let error = f(&mut out);
    let text = String::from(out);
    match error {
        None => format!("ok:{text}"),
        Some(error) => {
            assert_eq!(text, "", "a failed call writes an empty output");
            // SAFETY: a static NUL-terminated string.
            let code = unsafe { CStr::from_ptr(ddog_ai_usage_error_code_name(Some(&error))) }
                .to_str()
                .unwrap();
            let message = error.message.to_string();
            let rendered = match error.code {
                AiUsageErrorCode::Internal => format!(
                    "internal:{}",
                    json!({"error": {"code": code, "message": message}})
                ),
                AiUsageErrorCode::NoMatch => "rejected:".to_string(),
                _ => format!(
                    "rejected:{}",
                    json!({"error": {"code": code, "message": message}})
                ),
            };
            ddog_ai_usage_error_free(Some(error));
            rendered
        }
    }
}

struct V2;

impl Api for V2 {
    fn project(&self, input: &str) -> String {
        // SAFETY: a valid slice and output.
        call(|out| unsafe { ddog_ai_usage_project(cs(input), out) })
    }

    fn batch(&self, request: &str) -> String {
        let mut batch = Handle::empty();
        // SAFETY: `batch` is written by `batch_new` and used until dropped.
        unsafe {
            assert!(ddog_ai_usage_batch_new(&mut batch).is_none());
            ddog_ai_usage_batch_set_writer(&batch, cs("gateway-1"), 7);
            let added = call(|out| ddog_ai_usage_batch_add_request(&batch, cs(request), out));
            let intake = call(|out| ddog_ai_usage_batch_take_intake(&batch, 1_700_000_000, out));
            ddog_ai_usage_error_free(ddog_ai_usage_batch_add_request(
                &batch,
                cs(request),
                ptr::null_mut(),
            ));
            let mut dropped = 0;
            let dogstatsd = call(|out| {
                ddog_ai_usage_batch_take_dogstatsd_text(
                    &batch,
                    cs("env:test,version:1"),
                    UDP_MAX_PAYLOAD,
                    &mut dropped,
                    out,
                )
            });
            ddog_ai_usage_batch_drop(&mut batch);
            format!("{added}\n{intake}\n{dogstatsd}\n{dropped}")
        }
    }

    fn parse(&self, format: &str, embeddings: bool, options: u32, body: &[u8]) -> String {
        // SAFETY: valid slices and output.
        call(|out| unsafe {
            ddog_ai_usage_parse_provider_response_with(
                cs(format),
                embeddings,
                options,
                ByteSlice::from(body),
                out,
            )
        })
    }

    fn stream(&self, format: &str, embeddings: bool, options: u32, body: &[u8]) -> String {
        let mut stream = Handle::empty();
        // SAFETY: `stream` is written by `stream_new` and released by `take`.
        unsafe {
            if let Some(error) =
                ddog_ai_usage_stream_new_with(cs(format), embeddings, options, &mut stream)
            {
                ddog_ai_usage_error_free(Some(error));
                return "rejected:".to_string();
            }
            for chunk in body.chunks(7) {
                assert!(ddog_ai_usage_stream_feed(&mut stream, ByteSlice::from(chunk)).is_none());
            }
            let outcome = ddog_ai_usage_stream_outcome(&stream) as u32;
            let taken = call(|out| ddog_ai_usage_stream_take(&mut stream, out));
            format!("{outcome}\n{taken}")
        }
    }

    fn known_metrics(&self, profile: &str) -> String {
        // SAFETY: a valid slice and output.
        call(|out| unsafe { ddog_ai_usage_known_metrics(cs(profile), out) })
    }

    fn provider_id_headers(&self, provider: &str) -> String {
        // SAFETY: a valid slice and output.
        call(|out| unsafe { ddog_ai_usage_provider_id_headers(cs(provider), out) })
    }

    fn provider_ids(&self, input: &str) -> String {
        // SAFETY: a valid slice and output.
        call(|out| unsafe { ddog_ai_usage_provider_ids(cs(input), out) })
    }

    fn provider_name(&self, name: &str) -> String {
        // SAFETY: a valid slice and output.
        call(|out| unsafe { ddog_ai_usage_provider_name(cs(name), out) })
    }

    fn operation_name(&self, route: &str) -> String {
        // SAFETY: a valid slice and output.
        call(|out| unsafe { ddog_ai_usage_operation_name(cs(route), out) })
    }

    fn error_type(&self, name: &str, status: u16) -> String {
        // SAFETY: a valid slice and output.
        call(|out| unsafe { ddog_ai_usage_error_type(cs(name), status, out) })
    }

    fn error_type_known(&self, name: &str, status: u16) -> String {
        // SAFETY: a valid slice and output.
        call(|out| unsafe { ddog_ai_usage_error_type_known(cs(name), status, out) })
    }

    fn tag_value(&self, value: &str) -> String {
        // SAFETY: a valid slice and output.
        call(|out| unsafe { ddog_ai_usage_datadog_tag_value(cs(value), out) })
    }
}

#[test]
#[cfg_attr(miri, ignore)] // reads hundreds of fixture files
fn outputs_are_those_of_abi_v1() {
    let golden: BTreeMap<String, String> =
        serde_json::from_slice(&fs::read(golden_path()).unwrap()).unwrap();
    let outputs = outputs(&V2);
    assert_eq!(
        outputs.keys().collect::<Vec<_>>(),
        golden.keys().collect::<Vec<_>>(),
        "the corpus changed; it must stay the one v1 was recorded with"
    );
    let mismatches: Vec<String> = outputs
        .iter()
        .filter(|(name, output)| golden[name.as_str()] != digest(output))
        .map(|(name, output)| format!("{name}: {output}"))
        .collect();
    assert!(
        mismatches.is_empty(),
        "{} of {} outputs differ from ABI v1:\n{}",
        mismatches.len(),
        outputs.len(),
        mismatches.join("\n")
    );
    // Rejections are among them, so codes and messages are compared too.
    assert!(outputs.values().any(|output| output.contains("rejected:{")));
}
