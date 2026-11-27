// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_config_core::{
    AliasOrder, ConfigSchema, DiagnosticKind, Entry, Error, InputValue, Layer, Number,
    NumberSyntax, Origin, ParsePolicy, Resolution, ResolveInputs, Transform, TransformContext,
    TransformRegistry, TransformResult, Value, ValueKind,
};

fn layer(origin: Origin, entries: &[(&str, &str)]) -> Layer {
    Layer {
        origin,
        entries: entries
            .iter()
            .map(|&(name, raw)| Entry {
                name: name.to_owned(),
                value: InputValue::Raw(raw.to_owned()),
                config_id: (origin == Origin::LocalStableConfig).then(|| "local-match".to_owned()),
            })
            .collect(),
        skip_empty: false,
        alias_order: AliasOrder::AsProvided,
    }
}

fn normalized_mode(value: Value, _: TransformContext<'_>) -> TransformResult {
    let Value::String(value) = value else {
        return TransformResult::Reject {
            diagnostic: Some("fixture expects a string".to_owned()),
        };
    };
    TransformResult::accept(Value::String(value.to_ascii_lowercase()))
}

#[test]
fn selects_and_validates_one_name_per_source() {
    let mut transforms = TransformRegistry::with_builtins();
    transforms
        .register(
            "fixture.normalize_mode",
            Transform::new(ValueKind::String, ValueKind::String, normalized_mode),
        )
        .unwrap();
    let schema = ConfigSchema::from_bytes(
        br#"{"version":"2","supportedConfigurations":{
            "DD_MODE":[{"type":"string","default":"INFO","aliases":["OLD_MODE"],
            "configurationNames":["logMode"],
            "allowed":{"pattern":"info|debug","ignoreCase":true,"validateDefault":true},
            "transform":"fixture.normalize_mode"}],
            "DD_SECRET":[{"type":"string","default":null,"sensitive":true}]
        }}"#,
        ParsePolicy::default(),
        &transforms,
    )
    .unwrap();

    let inputs = ResolveInputs {
        layers: vec![
            layer(Origin::LocalStableConfig, &[("OLD_MODE", "DEBUG")]),
            layer(
                Origin::EnvVar,
                &[
                    ("OLD_MODE", "INFO"),
                    ("DD_MODE", "invalid"),
                    ("DD_SECRET", "private"),
                ],
            ),
            layer(Origin::FleetStableConfig, &[("DD_MODE", "bad")]),
        ],
        report_sources: true,
        ..ResolveInputs::default()
    };
    let Resolution::Complete(config) = schema.resolve(inputs).unwrap() else {
        panic!("all transforms are bound in Rust");
    };
    let mode = config.get("DD_MODE").unwrap();
    assert_eq!(mode.value.as_str(), Some("debug"));
    assert_eq!(mode.origin, Origin::LocalStableConfig);
    assert_eq!(mode.input_name.as_deref(), Some("OLD_MODE"));
    assert_eq!(mode.config_id.as_deref(), Some("local-match"));
    assert_eq!(config.get("OLD_MODE"), Some(mode));
    assert_eq!(config.settings().next().unwrap().property, "logMode");
    assert_eq!(
        config.get("DD_SECRET").unwrap().value.as_str(),
        Some("private")
    );
    assert_eq!(config.diagnostics().len(), 2);
    assert_eq!(config.diagnostics()[0].origin, Origin::EnvVar);
    assert_eq!(
        config.diagnostics()[0].problem,
        DiagnosticKind::DisallowedValue
    );
    assert_eq!(config.source_events().len(), 4);
    let events = config.source_events();
    assert_eq!(
        events[0].parsed.as_ref().and_then(Value::as_str),
        Some("INFO")
    );
    assert_eq!(
        events[1].parsed.as_ref().and_then(Value::as_str),
        Some("DEBUG")
    );
    assert_eq!(
        events[2].parsed.as_ref().and_then(Value::as_str),
        Some("invalid")
    );
    assert_eq!(events[2].value, None);
    assert!(
        !config
            .source_events()
            .iter()
            .any(|event| event.name == "DD_SECRET")
    );
}

#[test]
fn batches_host_values_and_resumes_in_source_order() {
    let mut transforms = TransformRegistry::default();
    transforms
        .register(
            "fixture.read",
            Transform::host(ValueKind::String, ValueKind::String, 9),
        )
        .unwrap();
    let schema = ConfigSchema::from_bytes(
        br#"{"version":"2","supportedConfigurations":{
            "DD_FILE":[{"type":"string","configurationNames":["path"],"default":null,
                "allowed":"/[^/]+","transform":"fixture.read"}],
            "DD_DEBUG":[{"type":"boolean","default":"false"}]
        }}"#,
        ParsePolicy::default(),
        &transforms,
    )
    .unwrap();
    let inputs = ResolveInputs {
        layers: vec![
            layer(Origin::LocalStableConfig, &[("DD_FILE", "/local")]),
            layer(
                Origin::EnvVar,
                &[("DD_FILE", "/missing"), ("DD_DEBUG", "1")],
            ),
            layer(Origin::FleetStableConfig, &[("DD_FILE", "denied")]),
        ],
        report_sources: true,
        ..ResolveInputs::default()
    };
    let Resolution::Pending(pending) = schema.resolve(inputs).unwrap() else {
        panic!("file reads require the caller");
    };
    let [first, second] = pending.requests() else {
        panic!("the disallowed path never reaches the transformer");
    };
    assert_eq!(first.slot, 9);
    assert_eq!(first.key, "DD_FILE");
    assert_eq!(first.value.as_str(), Some("/local"));
    assert_eq!(second.origin, Origin::EnvVar);
    assert_eq!(second.value.as_str(), Some("/missing"));

    let config = pending
        .resume(vec![
            TransformResult::accept(Value::String("contents".to_owned())),
            TransformResult::Reject {
                diagnostic: Some("missing".to_owned()),
            },
        ])
        .unwrap();
    let file = config.get("DD_FILE").unwrap();
    assert_eq!(file.value.as_str(), Some("contents"));
    assert_eq!(file.origin, Origin::LocalStableConfig);
    assert_eq!(config.get("DD_DEBUG").unwrap().value.as_bool(), Some(true));
    assert_eq!(
        config.diagnostics()[0].problem,
        DiagnosticKind::TransformRejected(Some("missing".to_owned()))
    );
    assert_eq!(
        config.diagnostics()[1].problem,
        DiagnosticKind::DisallowedValue
    );
}

#[test]
fn schema_rejects_unknown_transform_and_invalid_regexp() {
    let transforms = TransformRegistry::default();
    let unbound = br#"{"version":"2","supportedConfigurations":{"DD_KEY":[
        {"type":"string","transform":"missing"}
    ]}}"#;
    let malformed_pattern = br#"{"version":"2","supportedConfigurations":{"DD_KEY":[
        {"type":"string","allowed":"["}
    ]}}"#;
    assert!(matches!(
        ConfigSchema::from_bytes(unbound, ParsePolicy::default(), &transforms),
        Err(Error::UnboundTransform { key, name }) if key == "DD_KEY" && name == "missing"
    ));
    assert!(matches!(
        ConfigSchema::from_bytes(malformed_pattern, ParsePolicy::default(), &transforms),
        Err(Error::Regex { key, .. }) if key == "DD_KEY"
    ));
}

#[test]
fn preserves_parsed_numbers_and_warns_on_adjustment() {
    let schema = ConfigSchema::from_bytes(
        br#"{"version":"2","supportedConfigurations":{
            "DD_RATE":[{"type":"decimal","default":"0.5","transform":"sampleRate",
                        "configurationNames":["rate"]}],
            "DD_LIMIT":[{"type":"int","default":"30","allowed":"[0-9]+"}]
        }}"#,
        ParsePolicy {
            numbers: NumberSyntax::EcmaScript,
            coerce_code: true,
            ..ParsePolicy::default()
        },
        &TransformRegistry::with_builtins(),
    )
    .unwrap();
    let Resolution::Complete(config) = schema
        .resolve(ResolveInputs {
            layers: vec![
                layer(
                    Origin::EnvVar,
                    &[("DD_LIMIT", "0x3e"), ("DD_RATE", "-0.75")],
                ),
                layer(Origin::Code, &[("rate", "Infinity")]),
            ],
            ..ResolveInputs::default()
        })
        .unwrap()
    else {
        panic!("sample rate is a built-in");
    };
    assert_eq!(
        config.get("DD_LIMIT").unwrap().value,
        Value::Number(Number::Float(62.0))
    );
    assert_eq!(
        config.get("DD_RATE").unwrap().value,
        Value::Number(Number::Float(1.0))
    );
    assert_eq!(config.get("DD_RATE").unwrap().origin, Origin::Code);
    assert_eq!(config.diagnostics().len(), 2);
    assert!(matches!(
        config.diagnostics()[0].problem,
        DiagnosticKind::TransformWarning(_)
    ));
    assert!(config.source_events().is_empty());
}

#[test]
fn can_choose_aliases_by_declaration_or_input_order() {
    let schema = ConfigSchema::from_bytes(
        br#"{"version":"2","supportedConfigurations":{
            "DD_MODE":[{"type":"string","aliases":["FIRST","SECOND","ROOT_MODE"]}],
            "ROOT_MODE":[{"type":"string","default":"fallback"}]
        }}"#,
        ParsePolicy::default(),
        &TransformRegistry::default(),
    )
    .unwrap();
    for (alias_order, expected) in [
        (AliasOrder::AsProvided, "two"),
        (AliasOrder::AsDeclared, "one"),
    ] {
        let mut environment = layer(
            Origin::EnvVar,
            &[
                ("SECOND", "two"),
                ("ROOT_MODE", "configured"),
                ("FIRST", "one"),
            ],
        );
        environment.alias_order = alias_order;
        let Resolution::Complete(config) = schema
            .resolve(ResolveInputs {
                layers: vec![environment],
                ..ResolveInputs::default()
            })
            .unwrap()
        else {
            panic!("no host transforms");
        };
        assert_eq!(
            config.get("DD_MODE").unwrap().value.as_str(),
            Some(expected)
        );
        assert_eq!(
            config.get("ROOT_MODE").unwrap().value.as_str(),
            Some("configured")
        );
    }
}

#[test]
fn checks_integer_transform_results() {
    let mut transforms = TransformRegistry::default();
    transforms
        .register(
            "fixture.count",
            Transform::host(ValueKind::Integer, ValueKind::Integer, 4),
        )
        .unwrap();
    let schema = ConfigSchema::from_bytes(
        br#"{"version":"2","supportedConfigurations":{
            "DD_COUNT":[{"type":"int","transform":"fixture.count"}]
        }}"#,
        ParsePolicy::default(),
        &transforms,
    )
    .unwrap();
    for (number, expected) in [(1.5, false), (2.0, true), (f64::NAN, false)] {
        let Resolution::Pending(pending) = schema
            .resolve(ResolveInputs {
                layers: vec![layer(Origin::EnvVar, &[("DD_COUNT", "2")])],
                ..ResolveInputs::default()
            })
            .unwrap()
        else {
            panic!("this transform is supplied by the host");
        };
        let result = pending.resume(vec![TransformResult::accept(Value::Number(Number::Float(
            number,
        )))]);
        if expected {
            assert_eq!(
                result.unwrap().get("DD_COUNT").unwrap().value,
                Value::Number(Number::Float(number))
            );
        } else {
            assert!(matches!(result, Err(Error::InvalidTransformResult(key)) if key == "DD_COUNT"));
        }
    }
}

#[test]
fn rejects_ambiguous_binding_names() {
    let registry = TransformRegistry::default();
    for (key, scope) in [("alias", "aliases"), ("option", "configurationNames")] {
        let manifest = serde_json::json!({
            "version": "2",
            "supportedConfigurations": {
                "DD_FIRST": [{ "type": "string", (scope): [key] }],
                "DD_SECOND": [{ "type": "string", (scope): [key] }],
            }
        });
        assert!(matches!(
            ConfigSchema::from_bytes(manifest.to_string().as_bytes(), ParsePolicy::default(), &registry),
            Err(Error::AmbiguousName(name)) if name == key
        ));
    }
}

#[test]
fn validates_the_allowed_regexp_input_type() {
    let config = br#"{"version":"2","supportedConfigurations":{
        "DD_ITEMS":[{"type":"array","default":["a","b"],"allowed":"a,b"}]
    }}"#;
    assert!(matches!(
        ConfigSchema::from_bytes(config, ParsePolicy::default(), &TransformRegistry::default()),
        Err(Error::UnsupportedValidationKind(key)) if key == "DD_ITEMS"
    ));
}

#[test]
fn parses_space_separated_tags_and_preserves_values_when_comma_separated() {
    let schema = ConfigSchema::from_bytes(
        br#"{"version":"2","supportedConfigurations":{
            "DD_TAGS":[{"type":"map","parse":{"mapSyntax":"colon_or_space_separated"}}],
            "DD_MODE":[{"type":"string","default":"test","transform":"toUpperCase"}]
        }}"#,
        ParsePolicy::default(),
        &TransformRegistry::with_builtins(),
    )
    .unwrap();
    for (supplied, expected) in [
        ("env:test service:Demo", "test"),
        ("env:test env,service:Demo", "test env"),
    ] {
        let Resolution::Complete(config) = schema
            .resolve(ResolveInputs {
                layers: vec![layer(Origin::EnvVar, &[("DD_TAGS", supplied)])],
                ..ResolveInputs::default()
            })
            .unwrap()
        else {
            panic!("upper case has a native binding");
        };
        let Value::Map(tags) = &config.get("DD_TAGS").unwrap().value else {
            panic!("tags are a map");
        };
        assert_eq!(tags.get("env").map(String::as_str), Some(expected));
        assert_eq!(tags.get("service").map(String::as_str), Some("Demo"));
        assert_eq!(config.get("DD_MODE").unwrap().value.as_str(), Some("TEST"));
    }
}

#[test]
fn resolves_layers_over_caller_owned_defaults() {
    let mut transforms = TransformRegistry::default();
    transforms
        .register(
            "fixture.lookup",
            Transform::host(ValueKind::String, ValueKind::String, 5),
        )
        .unwrap();
    let schema = ConfigSchema::from_bytes(
        br#"{"version":"2","supportedConfigurations":{
            "DD_LOOKUP":[{"type":"string","default":"test","transform":"fixture.lookup"}],
            "DD_DEBUG":[{"type":"boolean","default":"false"}]
        }}"#,
        ParsePolicy::default(),
        &transforms,
    )
    .unwrap();
    let Resolution::Complete(snapshot) = schema
        .resolve(ResolveInputs {
            layers: vec![layer(Origin::EnvVar, &[("DD_DEBUG", "true")])],
            skip_defaults: true,
            report_sources: true,
        })
        .unwrap()
    else {
        panic!("the default transformer was skipped");
    };
    assert!(snapshot.get("DD_LOOKUP").is_none());
    assert_eq!(
        snapshot.get("DD_DEBUG").unwrap().value.as_bool(),
        Some(true)
    );
    let [debug] = snapshot.source_events() else {
        panic!("only the explicit environment setting was read");
    };
    assert!(debug.parsed.is_none());
    assert_eq!(debug.origin, Origin::EnvVar);
}
