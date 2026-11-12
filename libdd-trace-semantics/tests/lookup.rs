// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Lookup behavior, ported from the Agent's `pkg/trace/semantics` tests where they apply.

extern crate alloc;

use alloc::borrow::Cow;
use std::collections::HashMap;

use libdd_trace_model::{AttributeValue, Attributes, StringData};
use libdd_trace_semantics::{Concept, lookup_f64, lookup_i64, lookup_str};

type V = AttributeValue<StringData>;

fn s(v: &str) -> V {
    V::String(v.to_owned())
}

/// Typed attributes, like a V1 span.
#[derive(Default)]
struct Typed(HashMap<&'static str, V>);

impl Typed {
    fn new<const N: usize>(attrs: [(&'static str, V); N]) -> Self {
        Self(attrs.into_iter().collect())
    }
}

impl Attributes for Typed {
    type Text = String;
    type Bytes = Vec<u8>;
    type Values = StringData;

    fn attribute_str(&self, key: &str) -> Option<&str> {
        self.0.get(key)?.as_str()
    }

    fn attribute_f64(&self, key: &str) -> Option<f64> {
        self.0.get(key)?.as_f64()
    }

    fn attribute_i64(&self, key: &str) -> Option<i64> {
        self.0.get(key)?.as_i64()
    }
}

/// Strings in `meta` and floats in `metrics`, like a v0.4 span.
#[derive(Default)]
struct V04 {
    meta: HashMap<&'static str, &'static str>,
    metrics: HashMap<&'static str, f64>,
}

impl V04 {
    fn new<const M: usize, const N: usize>(
        meta: [(&'static str, &'static str); M],
        metrics: [(&'static str, f64); N],
    ) -> Self {
        Self {
            meta: meta.into_iter().collect(),
            metrics: metrics.into_iter().collect(),
        }
    }
}

impl Attributes for V04 {
    type Text = String;
    type Bytes = Vec<u8>;
    type Values = StringData;

    fn attribute_str(&self, key: &str) -> Option<&str> {
        self.meta.get(key).copied()
    }

    fn attribute_f64(&self, key: &str) -> Option<f64> {
        self.metrics.get(key).copied()
    }
}

#[test]
fn string_lookup_follows_precedence() {
    let both = Typed::new([
        ("http.method", s("GET")),
        ("http.request.method", s("POST")),
    ]);
    assert_eq!(
        lookup_str(&both, Concept::HttpMethod).as_deref(),
        Some("POST")
    );

    let dd_only = Typed::new([("http.method", s("GET"))]);
    assert_eq!(
        lookup_str(&dd_only, Concept::HttpMethod).as_deref(),
        Some("GET")
    );

    assert_eq!(lookup_str(&Typed::default(), Concept::HttpMethod), None);
}

#[test]
fn string_lookup_skips_empty_strings() {
    let attrs = Typed::new([("http.request.method", s("")), ("http.method", s("GET"))]);
    assert_eq!(
        lookup_str(&attrs, Concept::HttpMethod).as_deref(),
        Some("GET")
    );
}

#[test]
fn string_lookup_borrows_strings() {
    let attrs = Typed::new([("http.method", s("GET"))]);
    assert!(matches!(
        lookup_str(&attrs, Concept::HttpMethod),
        Some(Cow::Borrowed("GET"))
    ));
}

#[test]
fn string_lookup_formats_numbers() {
    // http.status_code: int64 first, then string.
    let int = Typed::new([("http.status_code", V::Int(200))]);
    assert_eq!(
        lookup_str(&int, Concept::HttpStatusCode).as_deref(),
        Some("200")
    );

    let v04 = V04::new([], [("http.status_code", 404.0)]);
    assert_eq!(
        lookup_str(&v04, Concept::HttpStatusCode).as_deref(),
        Some("404")
    );

    // _dd.top_level is float64.
    let float = Typed::new([("_dd.top_level", V::Float(1.0))]);
    assert_eq!(
        lookup_str(&float, Concept::DdTopLevel).as_deref(),
        Some("1")
    );
}

#[test]
fn string_lookup_ignores_values_of_another_type() {
    // http.method only has string entries.
    let attrs = Typed::new([
        ("http.request.method", V::Int(1)),
        ("http.method", s("GET")),
    ]);
    assert_eq!(
        lookup_str(&attrs, Concept::HttpMethod).as_deref(),
        Some("GET")
    );
}

#[test]
fn int_lookup_reads_typed_values_and_strings() {
    let int = Typed::new([("http.status_code", V::Int(200))]);
    assert_eq!(lookup_i64(&int, Concept::HttpStatusCode), Some(200));

    // A v0.4 meta string is reached through the string entry.
    let meta = V04::new([("http.status_code", "200")], []);
    assert_eq!(lookup_i64(&meta, Concept::HttpStatusCode), Some(200));

    let metrics = V04::new([], [("http.status_code", 200.0)]);
    assert_eq!(lookup_i64(&metrics, Concept::HttpStatusCode), Some(200));

    let float_string = V04::new([("http.status_code", "200.0")], []);
    assert_eq!(
        lookup_i64(&float_string, Concept::HttpStatusCode),
        Some(200)
    );

    let big = Typed::new([("http.status_code", V::Int(i64::MAX))]);
    assert_eq!(lookup_i64(&big, Concept::HttpStatusCode), Some(i64::MAX));
}

#[test]
fn int_lookup_falls_through_invalid_values() {
    // Each invalid http.status_code value is skipped in favor of http.response.status_code.
    for (meta, metric) in [
        (None, Some(f64::NAN)),
        (None, Some(1.5)),
        (None, Some(f64::INFINITY)),
        (None, Some(1e300)),
        (Some("abc"), None),
        (Some("1.5"), None),
        (Some(""), None),
    ] {
        let mut attrs = V04::new([("http.response.status_code", "418")], []);
        if let Some(meta) = meta {
            attrs.meta.insert("http.status_code", meta);
        }
        if let Some(metric) = metric {
            attrs.metrics.insert("http.status_code", metric);
        }
        assert_eq!(
            lookup_i64(&attrs, Concept::HttpStatusCode),
            Some(418),
            "meta {meta:?}, metric {metric:?}"
        );
    }
}

#[test]
fn float_lookup() {
    let int = Typed::new([("http.status_code", V::Int(200))]);
    assert_eq!(lookup_f64(&int, Concept::HttpStatusCode), Some(200.0));

    let meta = V04::new([("http.status_code", "200")], []);
    assert_eq!(lookup_f64(&meta, Concept::HttpStatusCode), Some(200.0));

    let top_level = V04::new([], [("_top_level", 1.0)]);
    assert_eq!(lookup_f64(&top_level, Concept::DdTopLevel), Some(1.0));

    let unparseable = V04::new([("http.status_code", "abc")], []);
    assert_eq!(lookup_f64(&unparseable, Concept::HttpStatusCode), None);

    // Like Go's ParseFloat: infinity literals are values, out of range strings are skipped.
    let inf = V04::new([("http.status_code", "Inf")], []);
    assert_eq!(
        lookup_f64(&inf, Concept::HttpStatusCode),
        Some(f64::INFINITY)
    );
    let nan = V04::new([("http.status_code", "NaN")], []);
    assert!(lookup_f64(&nan, Concept::HttpStatusCode).is_some_and(f64::is_nan));
    let out_of_range = V04::new(
        [
            ("http.status_code", "1e400"),
            ("http.response.status_code", "418"),
        ],
        [],
    );
    assert_eq!(
        lookup_f64(&out_of_range, Concept::HttpStatusCode),
        Some(418.0)
    );
    let signed_nan = V04::new(
        [
            ("http.status_code", "-nan"),
            ("http.response.status_code", "418"),
        ],
        [],
    );
    assert_eq!(
        lookup_f64(&signed_nan, Concept::HttpStatusCode),
        Some(418.0)
    );
    let double_sign = V04::new(
        [
            ("http.status_code", "++1"),
            ("http.response.status_code", "418"),
        ],
        [],
    );
    assert_eq!(
        lookup_f64(&double_sign, Concept::HttpStatusCode),
        Some(418.0)
    );
    assert_eq!(lookup_i64(&double_sign, Concept::HttpStatusCode), Some(418));
}

#[test]
fn float_entries_read_large_integers_exactly() {
    // _dd.top_level is a float64 entry. Above 2^53, going through f64 would round.
    let big = Typed::new([("_dd.top_level", V::Int(9_007_199_254_740_993))]);
    assert_eq!(
        lookup_i64(&big, Concept::DdTopLevel),
        Some(9_007_199_254_740_993)
    );
}

#[test]
fn float_entries_also_match_integers() {
    // Known difference with the Agent's V1 accessor, which only matches doubles for float64
    // entries and would return 0 here. See the lookup module docs.
    let attrs = Typed::new([("_dd.top_level", V::Int(1)), ("_top_level", V::Float(0.0))]);
    assert_eq!(lookup_i64(&attrs, Concept::DdTopLevel), Some(1));
}

#[test]
fn grpc_status_code_conditional_fallback() {
    // Same cases as the Agent's TestGRPCStatusCodeConditionalFallback.
    let cases: [(&str, Typed, Option<&str>); 7] = [
        (
            "explicit rpc.grpc.status_code wins regardless of system",
            Typed::new([
                ("rpc.grpc.status_code", s("3")),
                ("rpc.response.status_code", s("4")),
                ("rpc.system.name", s("jsonrpc")),
            ]),
            Some("3"),
        ),
        (
            "rpc.response.status_code accepted when rpc.system.name=grpc",
            Typed::new([
                ("rpc.response.status_code", s("DEADLINE_EXCEEDED")),
                ("rpc.system.name", s("grpc")),
            ]),
            Some("DEADLINE_EXCEEDED"),
        ),
        (
            "rpc.response.status_code accepted via legacy rpc.system=grpc",
            Typed::new([
                ("rpc.response.status_code", s("DEADLINE_EXCEEDED")),
                ("rpc.system", s("grpc")),
            ]),
            Some("DEADLINE_EXCEEDED"),
        ),
        (
            "rpc.system.name=grpc still accepted when legacy rpc.system disagrees",
            Typed::new([
                ("rpc.response.status_code", s("OK")),
                ("rpc.system.name", s("grpc")),
                ("rpc.system", s("jsonrpc")),
            ]),
            Some("OK"),
        ),
        (
            "non-gRPC rpc.system.name rejects rpc.response.status_code",
            Typed::new([
                ("rpc.response.status_code", s("-32602")),
                ("rpc.system.name", s("jsonrpc")),
            ]),
            None,
        ),
        (
            "explicit non-grpc rpc.system.name overrides legacy rpc.system=grpc",
            Typed::new([
                ("rpc.response.status_code", s("-32602")),
                ("rpc.system.name", s("jsonrpc")),
                ("rpc.system", s("grpc")),
            ]),
            None,
        ),
        (
            "int64 rpc.response.status_code is formatted as string",
            Typed::new([
                ("rpc.response.status_code", V::Int(7)),
                ("rpc.system.name", s("grpc")),
            ]),
            Some("7"),
        ),
    ];
    for (name, attrs, want) in cases {
        assert_eq!(
            lookup_str(&attrs, Concept::RpcGrpcStatusCode).as_deref(),
            want,
            "{name}"
        );
    }

    let typed = Typed::new([
        ("rpc.response.status_code", V::Int(7)),
        ("rpc.system.name", s("grpc")),
    ]);
    assert_eq!(lookup_i64(&typed, Concept::RpcGrpcStatusCode), Some(7));
}

#[test]
fn conditions_treat_empty_strings_as_absent() {
    // rpc.system.name is empty, so the legacy rpc.system=grpc branch applies.
    let attrs = Typed::new([
        ("rpc.response.status_code", s("OK")),
        ("rpc.system.name", s("")),
        ("rpc.system", s("grpc")),
    ]);
    assert_eq!(
        lookup_str(&attrs, Concept::RpcGrpcStatusCode).as_deref(),
        Some("OK")
    );
}
