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
fn resolves_a_manifest_once_using_active_rules_and_preserves_the_installed_schema() {
    let mut resolver = WireResolver::default();
    let previous = invoke(
        &mut resolver,
        json!({
            "operation": "initialize",
            "manifest": {"kind": "json", "value": r#"{"version":"2","supportedConfigurations":{
                "DD_PREVIOUS":[{"type":"string","default":"previous"}]
            }}"#},
        }),
    )
    .unwrap();
    assert_eq!(previous["status"], "complete");

    let mut request = json!({
        "operation": "resolve_once",
        "manifest": {"kind": "json", "value": r#"{"version":"2","supportedConfigurations":{
            "DD_CANONICAL":[{"type":"string","aliases":["DD_ALIAS"],"configurationNames":["name"]}],
                "DD_ALIAS":[{"type":"string","aliasOnly":true}],
            "DD_CODE":[{"type":"int","configurationNames":["limit"],"default":"invalid"}],
            "DD_DORMANT":[{"type":"string","allowed":"["}]
        }}"#},
        "inputs": {
            "skip_defaults": true,
            "layers": [
                {"origin": "env_var", "entries": [{"name": "DD_ALIAS", "value": "env"}]},
                {"origin": "code", "entries": [
                    {"name": "limit", "value": {"kind": "number", "value": 5}}
                ]}
            ],
        },
    });
    let snapshot = invoke(&mut resolver, request.clone()).unwrap();
    assert_eq!(snapshot["status"], "complete", "{snapshot}");
    let settings = snapshot["settings"].as_array().unwrap();
    assert_eq!(settings.len(), 2);
    assert_eq!(settings[0]["resolved"]["input_name"], "DD_ALIAS");
    assert_eq!(settings[0]["resolved"]["value"]["value"], "env");
    assert_eq!(settings[1]["resolved"]["value"]["value"], 5);

    request["inputs"]["skip_defaults"] = false.into();
    let invalid = invoke(&mut resolver, request).unwrap();
    assert_eq!(invalid["status"], "error");
    assert!(invalid["message"].as_str().unwrap().contains("DD_CODE"));

    let saved = invoke(&mut resolver, json!({"operation": "resolve", "inputs": {}})).unwrap();
    assert_eq!(saved, previous);
}

#[test]
fn one_shot_skips_aliases_shadowed_by_the_supplied_canonical_name() {
    let mut resolver = WireResolver::default();
    let result = invoke(
        &mut resolver,
        json!({
            "operation": "resolve_once",
            "manifest": {"kind": "json", "value": r#"{"version":"2","supportedConfigurations":{
                "DD_MODE":[{"type":"string"}],
                "DD_MODE_EXTRACT":[{"type":"string","aliases":["DD_MODE"],"transform":"fixture.unbound"}]
            }}"#},
            "inputs": {
                "skip_defaults": true,
                "layers": [{"origin":"env_var","entries":[{"name":"DD_MODE","value":"test"}]}],
            },
        }),
    )
    .unwrap();
    assert_eq!(result["status"], "complete", "{result}");
    let settings = result["settings"].as_array().unwrap();
    assert_eq!(settings.len(), 1);
    assert_eq!(settings[0]["name"], "DD_MODE");
    assert_eq!(settings[0]["resolved"]["value"]["value"], "test");
}

#[test]
fn one_shot_drops_unrequested_aliases_shadowed_in_the_full_schema() {
    let mut resolver = WireResolver::default();
    let mut request = json!({
        "operation": "initialize",
        "manifest": {"kind": "json", "value": r#"{"version":"2","supportedConfigurations":{
            "DD_STYLE":[{"type":"string"}],
            "DD_STYLE_EXTRACT":[{"type":"string","aliases":["DD_STYLE"]}],
            "DD_STYLE_INJECT":[{"type":"string","aliases":["DD_STYLE"]}]
        }}"#},
        "inputs": {
            "skip_defaults": true,
            "layers": [{"origin":"env_var","entries":[
                {"name":"DD_STYLE_EXTRACT","value":"datadog"},
                {"name":"DD_STYLE_INJECT","value":"tracecontext"}
            ]}],
        },
    });
    let full = invoke(&mut resolver, request.clone()).unwrap();
    assert_eq!(full["status"], "complete", "{full}");

    request["operation"] = "resolve_once".into();
    let selected = invoke(&mut resolver, request).unwrap();
    assert_eq!(selected["status"], "complete", "{selected}");
    assert_eq!(
        selected["settings"],
        json!(full["settings"].as_array().unwrap()[1..])
    );
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
fn reports_the_supplied_spelling_for_each_programmatic_attempt() {
    let result = initialize(
        r#"{"version":"2","supportedConfigurations":{
            "DD_TRACE_SAMPLE_RATE":[{"type":"decimal","transform":"sampleRate",
                "configurationNames":["sampleRate","ingestion.sampleRate"]}]
        }}"#,
        &json!({"skip_defaults":true,"report_sources":true,"layers":[
            {"origin":"env_var","entries":[{"name":"DD_TRACE_SAMPLE_RATE","value":"invalid"}]},
            {"origin":"code","entries":[
                {"name":"sampleRate","value":{"kind":"number","value":1.2}},
                {"name":"ingestion.sampleRate","value":{"kind":"number","value":-0.5}}
            ]}
        ]}),
        &json!([]),
    )
    .unwrap();
    assert_eq!(result["status"], "complete");
    let [canonical, first, second] = result["sources"].as_array().unwrap().as_slice() else {
        panic!("all attempts must be reported");
    };
    assert_eq!(canonical["name"], "DD_TRACE_SAMPLE_RATE");
    assert!(canonical.get("input_name").is_none());
    assert_eq!(first["name"], "DD_TRACE_SAMPLE_RATE");
    assert_eq!(first["input_name"], "sampleRate");
    assert_eq!(first["parsed"]["value"], 1.2);
    assert_eq!(first["value"]["value"], 1.0);
    assert_eq!(second["name"], "DD_TRACE_SAMPLE_RATE");
    assert_eq!(second["input_name"], "ingestion.sampleRate");
    assert_eq!(second["parsed"]["value"], -0.5);
    assert_eq!(second["value"]["value"], 0.0);
    let [canonical, first, second] = result["diagnostics"].as_array().unwrap().as_slice() else {
        panic!("all attempts must have a diagnostic");
    };
    assert!(canonical.get("input_name").is_none());
    assert_eq!(canonical["problem"]["kind"], "invalid_value");
    assert_eq!(first["input_name"], "sampleRate");
    assert_eq!(
        first["problem"]["message"],
        "Sample rate out of range between 0 and 1"
    );
    assert_eq!(second["input_name"], "ingestion.sampleRate");
    assert_eq!(
        result["settings"][0]["resolved"]["input_name"],
        "ingestion.sampleRate"
    );
}

#[test]
fn reports_configuration_ids_for_overridden_and_rejected_attempts() {
    let result = initialize(
        r#"{"version":"2","supportedConfigurations":{
            "DD_MODE":[{"type":"string","default":"info","allowed":"info|debug",
                "configurationNames":["logMode"]}]
        }}"#,
        &json!({"report_sources":true,"layers":[
            {"origin":"local_stable_config","entries":[
                {"name":"DD_MODE","value":"debug","config_id":"local-policy"}
            ]},
            {"origin":"fleet_stable_config","entries":[
                {"name":"DD_MODE","value":"invalid","config_id":"fleet-policy"}
            ]},
            {"origin":"code","entries":[
                {"name":"logMode","value":{"kind":"string","value":"info"}}
            ]}
        ]}),
        &json!([]),
    )
    .unwrap();
    assert_eq!(result["status"], "complete");
    let [default, local, fleet, code] = result["sources"].as_array().unwrap().as_slice() else {
        panic!("all attempts must be reported");
    };
    assert_eq!(local["config_id"], "local-policy");
    assert_eq!(local["value"]["value"], "debug");
    assert_eq!(fleet["config_id"], "fleet-policy");
    assert!(fleet["value"].is_null());
    assert!(default.get("config_id").is_none());
    assert!(code.get("config_id").is_none());
    let winner = &result["settings"][0]["resolved"];
    assert_eq!(winner["origin"], "code");
    assert!(winner.get("config_id").is_none());
}

#[test]
fn formats_ecmascript_numbers_for_string_coercion_and_allowed_patterns() {
    let mut resolver = WireResolver::default();
    let midpoint = 1_571_277_825_824_000.0 + 0.25;
    let result = invoke(
        &mut resolver,
        json!({
            "operation": "initialize",
            "manifest": {"kind": "json", "value": r#"{"version":"2","supportedConfigurations":{
                "DD_TEXT":[{"type":"string","configurationNames":["text"]}],
                "DD_DECIMAL":[{"type":"decimal","allowed":"1571277825824000\\.2"}]
            }}"#},
            "policy": {"numbers": "ecma_script", "coerce_code": true},
            "inputs": {"layers": [{"origin": "env_var", "entries": [
                {"name": "DD_DECIMAL", "value": "1571277825824000.25"}
            ]}]},
        }),
    )
    .unwrap();
    assert_eq!(
        result["settings"][1]["resolved"]["value"]["value"],
        midpoint
    );
    assert_eq!(result["diagnostics"], json!([]));

    for (number, expected) in [
        (midpoint, "1571277825824000.2"),
        (-midpoint, "-1571277825824000.2"),
        (-0.0, "0"),
        (1e-7, "1e-7"),
        (1e-6, "0.000001"),
        (1e20, "100000000000000000000"),
        (1e21, "1e+21"),
    ] {
        let result = invoke(
            &mut resolver,
            json!({
                "operation": "resolve",
                "inputs": {"layers": [{"origin": "code", "entries": [
                    {"name": "text", "value": {"kind": "number", "value": number}}
                ]}]},
            }),
        )
        .unwrap();
        assert_eq!(
            result["settings"][0]["resolved"]["value"]["value"],
            expected
        );
    }
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
    assert!(reply["diagnostics"][1].get("input_name").is_none());
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
