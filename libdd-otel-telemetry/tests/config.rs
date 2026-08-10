// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_otel_telemetry::{parse_otlp_headers, OtlpProtocol, Temporality};

#[test]
fn protocol_parses_case_insensitively() {
    assert_eq!(
        OtlpProtocol::from_config_str("grpc"),
        Some(OtlpProtocol::Grpc)
    );
    assert_eq!(
        OtlpProtocol::from_config_str("GRPC"),
        Some(OtlpProtocol::Grpc)
    );
    assert_eq!(
        OtlpProtocol::from_config_str("  http/protobuf  "),
        Some(OtlpProtocol::HttpProtobuf)
    );
    assert_eq!(
        OtlpProtocol::from_config_str("HTTP/PROTOBUF"),
        Some(OtlpProtocol::HttpProtobuf)
    );
    assert_eq!(
        OtlpProtocol::from_config_str("http/json"),
        Some(OtlpProtocol::HttpJson)
    );
}

#[test]
fn protocol_returns_none_for_unknown_or_empty() {
    assert_eq!(OtlpProtocol::from_config_str(""), None);
    assert_eq!(OtlpProtocol::from_config_str("thrift"), None);
}

#[test]
fn temporality_parses_case_insensitively_and_defaults_to_delta() {
    assert_eq!(Temporality::from_config_str("delta"), Temporality::Delta);
    assert_eq!(Temporality::from_config_str("DELTA"), Temporality::Delta);
    assert_eq!(
        Temporality::from_config_str("Cumulative"),
        Temporality::Cumulative
    );
    assert_eq!(
        Temporality::from_config_str("  CUMULATIVE  "),
        Temporality::Cumulative
    );
    // Empty and unknown default to Delta.
    assert_eq!(Temporality::from_config_str(""), Temporality::Delta);
    assert_eq!(
        Temporality::from_config_str("lowmemory"),
        Temporality::Delta
    );
}

#[test]
fn headers_parse_key_value_pairs() {
    assert_eq!(
        parse_otlp_headers("k1=v1,k2=v2"),
        vec![
            ("k1".to_string(), "v1".to_string()),
            ("k2".to_string(), "v2".to_string())
        ]
    );
}

#[test]
fn headers_trim_and_skip_malformed_entries() {
    assert_eq!(
        parse_otlp_headers(" api-key = secret , , novalue , =dangling , k=v "),
        vec![
            ("api-key".to_string(), "secret".to_string()),
            ("k".to_string(), "v".to_string())
        ]
    );
    assert!(parse_otlp_headers("").is_empty());
}
