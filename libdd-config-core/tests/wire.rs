// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_config_core::{ConfigSchema, ParsePolicy, TransformRegistry, WireResolver};
use serde_json::{Value, json};

fn initialize(
    manifest: &str,
    inputs: &Value,
    host_transforms: &Value,
) -> serde_json::Result<Value> {
    let mut resolver = WireResolver::default();
    invoke(
        &mut resolver,
        json!({
            "operation": "initialize",
            "manifest": {"kind": "json", "value": manifest},
            "policy": {"booleans": "go"},
            "inputs": inputs,
            "host_transforms": host_transforms,
        }),
    )
}

fn invoke(resolver: &mut WireResolver, mut request: Value) -> serde_json::Result<Value> {
    request["version"] = 1.into();
    let bytes = serde_json::to_vec(&request)?;
    serde_json::from_slice(&resolver.request(&bytes)?)
}

#[test]
fn applies_parser_policy_on_raw_and_typed_inputs() {
    let result = initialize(
        r#"{"version":"2","supportedConfigurations":{
            "DD_BOOL":[{"type":"boolean","default":"false"}],
            "DD_ITEMS":[{"type":"array","parse":{"preserveEmptyItems":true}}],
            "DD_MAP":[{"type":"map","parse":{"mapSyntax":"equals_percent_decoded"}}],
            "DD_JSON":[{"type":"json","default":"{}"}],
            "DD_LIMIT":[{"type":"int","configurationNames":["limit"]}]
        }}"#,
        &json!({"layers":[
            {"origin":"env_var","entries":[
                {"name":"DD_BOOL","value":"t"},
                {"name":"DD_ITEMS","value":"first, ,last,"},
                {"name":"DD_MAP","value":"first%20key=a%2Fb,tag=on"},
                {"name":"DD_JSON","value":"[\"kept\"]"}
            ]},
            {"origin":"fleet_stable_config","skip_empty":true,"entries":[
                {"name":"DD_BOOL","value":""},{"name":"DD_JSON","value":"{"}
            ]},
            {"origin":"code","entries":[{"name":"limit","value":{"kind":"number","value":2.5}}]}
        ]}),
        &json!([]),
    )
    .unwrap();
    assert_eq!(result["status"], "complete");
    let settings = result["settings"].as_array().unwrap();
    assert_eq!(settings[0]["resolved"]["value"]["value"], true);
    assert_eq!(
        settings[1]["resolved"]["value"]["value"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["value"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["first", "", "last", ""],
    );
    assert_eq!(
        settings[2]["resolved"]["value"]["value"]["first key"],
        "a/b"
    );
    assert_eq!(settings[3]["resolved"]["value"]["value"], "[\"kept\"]");
    assert_eq!(settings[4]["resolved"]["value"]["value"], 2.0);
    assert_eq!(settings[4]["resolved"]["origin"], "code");
    assert_eq!(result["diagnostics"].as_array().unwrap().len(), 1);
    assert_eq!(result["diagnostics"][0]["name"], "DD_JSON");
}

#[test]
fn reports_later_unknown_options_after_pending_diagnostics() {
    let mut resolver = WireResolver::default();
    let pending = invoke(
        &mut resolver,
        json!({
            "operation":"initialize",
            "manifest":{"kind":"json","value":r#"{"version":"2","supportedConfigurations":{
            "DD_SECRET":[{"type":"string","sensitive":true,"transform":"fixture.secret"}]
        }}"#},
            "host_transforms":[{"name":"fixture.secret","input":"string","output":"string","slot":6}],
            "inputs":{"report_sources":true,"layers":[
                {"origin":"env_var","entries":[{"name":"DD_SECRET","value":"private"}]},
                {"origin":"code","entries":[{"name":"typo","value":"extra"}]}
            ]}
        }),
    );
    let pending = pending.unwrap();
    assert_eq!(pending["status"], "pending");
    assert_eq!(pending["requests"][0]["slot"], 6);
    let reply = invoke(
        &mut resolver,
        json!({
            "operation":"resume",
            "outcomes":[{"status":"reject","diagnostic":"contains private input"}]
        }),
    );
    let reply = reply.unwrap();
    assert_eq!(reply["status"], "complete");
    assert_eq!(reply["diagnostics"][0]["name"], "DD_SECRET");
    assert_eq!(
        reply["diagnostics"][0]["problem"]["kind"],
        "transform_rejected"
    );
    assert!(reply["diagnostics"][0]["problem"]["message"].is_null());
    assert_eq!(reply["diagnostics"][1]["problem"]["kind"], "unknown_option");
    assert_eq!(reply["sources"], json!([]));
    assert!(reply["settings"][0]["resolved"].is_null());
}

#[test]
fn accepts_prepared_manifest_from_path() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/supported-configurations.json");
    let mut registry = TransformRegistry::with_builtins();
    registry
        .register(
            "fixture.read",
            libdd_config_core::Transform::host(
                libdd_config_core::ValueKind::String,
                libdd_config_core::ValueKind::String,
                1,
            ),
        )
        .unwrap();
    assert!(ConfigSchema::from_path(manifest, ParsePolicy::default(), &registry).is_ok());
}

#[test]
fn rejects_malformed_json_supplied_as_a_typed_value() {
    let reply = initialize(
        r#"{"version":"2","supportedConfigurations":{
            "DD_JSON":[{"type":"json","default":"{}"}],
            "DD_ARRAY":[{"type":"array","default":"original"}]
        }}"#,
        &json!({"layers":[{"origin":"env_var","entries":[
            {"name":"DD_JSON","value":{"kind":"json","value":"{"}},
            {"name":"DD_ARRAY","value":{"kind":"array","value":[{"kind":"json","value":"{"}]}}
        ]}]}),
        &json!([]),
    )
    .unwrap();
    assert_eq!(reply["status"], "complete");
    assert_eq!(reply["settings"][0]["resolved"]["origin"], "default");
    assert_eq!(reply["settings"][0]["resolved"]["value"]["value"], "{}");
    assert_eq!(reply["settings"][1]["resolved"]["origin"], "default");
    assert_eq!(reply["diagnostics"].as_array().unwrap().len(), 2);
}

#[test]
fn rejects_malformed_json_returned_by_a_host_transform() {
    let mut resolver = WireResolver::default();
    let pending = invoke(&mut resolver, json!({
        "operation":"initialize",
        "manifest":{"kind":"json","value":r#"{"version":"2","supportedConfigurations":{
            "DD_JSON":[{"type":"string","transform":"fixture.json"}]
        }}"#},
        "host_transforms":[{"name":"fixture.json","input":"string","output":"json","slot":3}],
        "inputs":{"layers":[{"origin":"env_var","entries":[{"name":"DD_JSON","value":"value"}]}]}
    })).unwrap();
    assert_eq!(pending["status"], "pending");
    let reply = invoke(
        &mut resolver,
        json!({
            "operation":"resume",
            "outcomes":[{"status":"accept","value":{"kind":"json","value":"{"}}]
        }),
    )
    .unwrap();
    assert_eq!(reply["status"], "error");
    assert_eq!(
        reply["message"],
        "transform returned an incompatible value for DD_JSON"
    );
}
