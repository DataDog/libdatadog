// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Maps client-computed span stats into `traces.span.sdk.metrics.duration` OTLP histograms.
//! DDSketch summaries from the span concentrator are bucketed into fixed explicit bounds (seconds).

use super::config::OtlpMetricsConfig;
use super::exporter::{send_otlp_http, OTLP_MAX_RETRIES, OTLP_SHUTDOWN_MAX_RETRIES};
use async_trait::async_trait;
use libdd_capabilities::{HttpClientCapability, MaybeSend, SleepCapability};
use libdd_ddsketch::DDSketch;
use libdd_shared_runtime::Worker;
use libdd_trace_protobuf::pb;
use libdd_trace_stats::span_concentrator::{OtlpStatsBucket, SpanConcentrator};
use libdd_trace_utils::otlp_encoder::mapper::status_code;
use libdd_trace_utils::otlp_encoder::OtlpResourceInfo;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::error;
use web_time::SystemTime;

const METRIC_NAME: &str = "traces.span.sdk.metrics.duration";
const NANOS_PER_SECOND: f64 = 1_000_000_000.0;

// Canonical gRPC status names indexed by numeric code.
// See <https://github.com/grpc/grpc/blob/master/doc/statuscodes.md>.
const GRPC_STATUS_NAMES: [&str; 17] = [
    "OK",
    "CANCELLED",
    "UNKNOWN",
    "INVALID_ARGUMENT",
    "DEADLINE_EXCEEDED",
    "NOT_FOUND",
    "ALREADY_EXISTS",
    "PERMISSION_DENIED",
    "RESOURCE_EXHAUSTED",
    "FAILED_PRECONDITION",
    "ABORTED",
    "OUT_OF_RANGE",
    "UNIMPLEMENTED",
    "INTERNAL",
    "UNAVAILABLE",
    "DATA_LOSS",
    "UNAUTHENTICATED",
];

fn grpc_status_code_to_name(code: &str) -> Option<&'static str> {
    GRPC_STATUS_NAMES.get(code.parse::<usize>().ok()?).copied()
}
/// Fixed bucket boundaries (seconds) mirroring the OTel spanmetrics-connector defaults.
const EXPLICIT_BOUNDS_SECONDS: [f64; 16] = [
    0.002, 0.004, 0.006, 0.008, 0.01, 0.05, 0.1, 0.2, 0.4, 0.8, 1.0, 1.4, 2.0, 5.0, 10.0, 15.0,
];

fn kv_str(key: &str, value: &str) -> Value {
    json!({ "key": key, "value": { "stringValue": value } })
}
fn kv_int(key: &str, value: i64) -> Value {
    json!({ "key": key, "value": { "intValue": value.to_string() } })
}

/// Build an OTLP metrics export request (`ExportMetricsServiceRequest`) as a JSON value.
///
/// Emits one histogram data point per (aggregation key, ok/error) cell with exact `count`,
/// `sum`, `min` and `max` (the explicit-bucket distribution is still projected from the
/// per-cell DDSketch). Returns `None` when no cell yields a data point.
pub fn map_stats_to_otlp_metrics(
    buckets: &[OtlpStatsBucket],
    resource_info: &OtlpResourceInfo,
    otel_trace_semantics_enabled: bool,
) -> Option<Value> {
    let mut data_points = Vec::new();
    for b in buckets {
        let end = b.bucket.start.saturating_add(b.bucket.duration);
        for (group, exact) in b.bucket.stats.iter().zip(b.exact.iter()) {
            for (is_error, summary, cell) in [
                (false, &group.ok_summary, &exact.ok),
                (true, &group.error_summary, &exact.error),
            ] {
                if cell.count == 0 {
                    continue;
                }
                let Some(sketch) = DDSketch::from_encoded(summary) else {
                    continue;
                };
                data_points.push(json!({
                    "attributes": build_attributes(group, is_error, resource_info, otel_trace_semantics_enabled),
                    "startTimeUnixNano": b.bucket.start.to_string(),
                    "timeUnixNano": end.to_string(),
                    "count": cell.count.to_string(),
                    "sum": ns_to_s(cell.duration_ns),
                    "bucketCounts": sketch_bucket_counts(&sketch),
                    "explicitBounds": EXPLICIT_BOUNDS_SECONDS,
                    "min": ns_to_s(cell.min_ns),
                    "max": ns_to_s(cell.max_ns),
                }));
            }
        }
    }
    if data_points.is_empty() {
        return None;
    }
    Some(json!({
        "resourceMetrics": [{
            "resource": { "attributes": build_resource_attributes(resource_info, otel_trace_semantics_enabled) },
            "scopeMetrics": [{
                "metrics": [{
                    "name": METRIC_NAME,
                    "unit": "s",
                    "histogram": {
                        // OTLP AggregationTemporality::Delta (each export covers only the interval).
                        "aggregationTemporality": 1,
                        "dataPoints": data_points,
                    }
                }]
            }]
        }]
    }))
}

fn ns_to_s(ns: u64) -> f64 {
    ns as f64 / NANOS_PER_SECOND
}

/// Project the sketch's bins onto [`EXPLICIT_BOUNDS_SECONDS`] (one bucket per boundary plus a
/// trailing overflow bucket). The exact `count`/`sum`/`min`/`max` come from the concentrator's
/// per-cell accumulators; this function only shapes the distribution.
fn sketch_bucket_counts(sketch: &DDSketch) -> Vec<String> {
    let mut weights = vec![0.0_f64; EXPLICIT_BOUNDS_SECONDS.len() + 1];
    for (value, weight) in sketch.ordered_bins() {
        if weight <= 0.0 {
            continue;
        }
        let seconds = value / NANOS_PER_SECOND;
        let idx = EXPLICIT_BOUNDS_SECONDS
            .iter()
            .position(|b| seconds <= *b)
            .unwrap_or(EXPLICIT_BOUNDS_SECONDS.len());
        weights[idx] += weight;
    }
    weights
        .iter()
        .map(|w| (w.round() as u64).to_string())
        .collect()
}

fn build_attributes(
    group: &pb::ClientGroupedStats,
    is_error: bool,
    resource_info: &OtlpResourceInfo,
    otel_trace_semantics_enabled: bool,
) -> Vec<Value> {
    let mut attrs = Vec::new();
    let mut push = |k: &str, v: &str| {
        if !v.is_empty() {
            attrs.push(kv_str(k, v));
        }
    };

    // Service identity is on the resource; emit on the data point only when overridden.
    let group_service = if group.service.is_empty() {
        resource_info.service.as_str()
    } else {
        group.service.as_str()
    };
    if group_service != resource_info.service {
        push("service.name", group_service);
    }

    push("span.name", &group.resource);
    push("span.kind", &group.span_kind);
    push("http.request.method", &group.http_method);
    push("http.route", &group.http_endpoint);
    // group.grpc_status_code is the numeric code as a string; emit the canonical OTel status name.
    if let Some(name) = grpc_status_code_to_name(&group.grpc_status_code) {
        push("rpc.response.status_code", name);
    }
    for tag in &group.peer_tags {
        if let Some((k, v)) = tag.split_once(':') {
            push(k, v);
        }
    }
    if !otel_trace_semantics_enabled {
        push("datadog.operation.name", &group.name);
        push("datadog.span.type", &group.r#type);
    }
    if group.http_status_code != 0 {
        attrs.push(kv_int(
            "http.response.status_code",
            group.http_status_code as i64,
        ));
    }
    if !otel_trace_semantics_enabled {
        // Only `synthetics` is surfaced as `datadog.origin`: the aggregation key carries just a
        // boolean, not the full origin string, so other origins are lost upstream.
        if group.synthetics {
            attrs.push(kv_str("datadog.origin", "synthetics"));
        }
        if group.is_trace_root == pb::Trilean::True as i32 {
            attrs.push(json!({ "key": "_datadog.is_trace_root", "value": { "boolValue": true } }));
        }
        let top_level = group.hits > 0 && group.top_level_hits == group.hits;
        attrs.push(json!({
            "key": "datadog.span.top_level", "value": { "boolValue": top_level }
        }));
    }
    if is_error {
        attrs.push(kv_int("status.code", status_code::ERROR as i64));
    }
    attrs
}

fn build_resource_attributes(
    info: &OtlpResourceInfo,
    otel_trace_semantics_enabled: bool,
) -> Vec<Value> {
    let mut attrs: Vec<Value> = [
        ("service.name", info.service.as_str()),
        ("service.version", info.app_version.as_str()),
        ("deployment.environment.name", info.env.as_str()),
        ("host.name", info.hostname.as_str()),
        ("telemetry.sdk.name", "datadog"),
        ("telemetry.sdk.language", info.language.as_str()),
        ("telemetry.sdk.version", info.tracer_version.as_str()),
    ]
    .into_iter()
    .filter(|(_, v)| !v.is_empty())
    .map(|(k, v)| kv_str(k, v))
    .collect();

    if !otel_trace_semantics_enabled {
        if !info.runtime_id.is_empty() {
            attrs.push(kv_str("datadog.runtime_id", &info.runtime_id));
        }
        attrs.extend(info.process_tags.split(',').filter_map(|p| {
            let (k, v) = p.split_once(':')?;
            let (k, v) = (k.trim(), v.trim());
            (!k.is_empty() && !v.is_empty()).then(|| kv_str(&format!("datadog.{k}"), v))
        }));
    }
    attrs
}

/// Worker that periodically flushes the span concentrator and exports OTLP trace metrics.
#[derive(Debug)]
pub struct OtlpStatsExporter<C: HttpClientCapability + SleepCapability> {
    pub flush_interval: Duration,
    pub concentrator: Arc<Mutex<SpanConcentrator>>,
    pub config: OtlpMetricsConfig,
    pub resource: OtlpResourceInfo,
    pub test_token: Option<String>,
    pub capabilities: C,
}

impl<C: HttpClientCapability + SleepCapability> OtlpStatsExporter<C> {
    /// Flush the concentrator and export stats; returns `Ok(true)` if anything was sent.
    async fn send(&self, force_flush: bool, max_retries: u32) -> anyhow::Result<bool> {
        let buckets = {
            #[allow(clippy::unwrap_used)]
            let mut c = self.concentrator.lock().unwrap();
            c.flush_with_otlp_exact(SystemTime::now(), force_flush)
        };
        if buckets.is_empty() {
            return Ok(false);
        }
        let Some(request) = map_stats_to_otlp_metrics(
            &buckets,
            &self.resource,
            self.config.otel_trace_semantics_enabled,
        ) else {
            return Ok(false);
        };
        send_otlp_http(
            &self.capabilities,
            &self.config.endpoint_url,
            &self.config.headers,
            self.config.timeout,
            self.test_token.as_deref(),
            libdd_common::header::APPLICATION_JSON,
            serde_json::to_vec(&request)?,
            max_retries,
        )
        .await?;
        Ok(true)
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<C: HttpClientCapability + SleepCapability + MaybeSend + Sync + 'static> Worker
    for OtlpStatsExporter<C>
{
    async fn trigger(&mut self) {
        self.capabilities.sleep(self.flush_interval).await;
    }

    async fn run(&mut self) {
        if let Err(e) = self.send(false, OTLP_MAX_RETRIES).await {
            error!(?e, "Error exporting OTLP trace metrics");
        }
    }

    async fn shutdown(&mut self) {
        // Single attempt: a long backoff could miss the bounded shutdown window.
        if let Err(e) = self.send(true, OTLP_SHUTDOWN_MAX_RETRIES).await {
            error!(?e, "Error exporting OTLP trace metrics on shutdown");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_trace_stats::span_concentrator::{OtlpExactCell, OtlpExactGroup};

    const T_START: u64 = 12_340_000_000_000;
    const T_SIZE: u64 = 10_000_000_000;

    fn sketch(points: &[f64]) -> Vec<u8> {
        let mut s = DDSketch::default();
        for p in points {
            s.add(*p).unwrap();
        }
        s.encode_to_vec()
    }

    fn resource() -> OtlpResourceInfo {
        let mut r = OtlpResourceInfo::default();
        r.service = "svc".to_string();
        r.env = "test".to_string();
        r
    }

    fn cell(durations_ns: &[u64]) -> OtlpExactCell {
        OtlpExactCell {
            count: durations_ns.len() as u64,
            duration_ns: durations_ns.iter().sum(),
            min_ns: durations_ns.iter().copied().min().unwrap_or(0),
            max_ns: durations_ns.iter().copied().max().unwrap_or(0),
        }
    }

    /// Build one (pb group, exact group) cell pair. `customize` lets each test tweak the pb group.
    fn group_with_exact(
        ok_ns: &[u64],
        err_ns: &[u64],
        customize: impl FnOnce(&mut pb::ClientGroupedStats),
    ) -> (pb::ClientGroupedStats, OtlpExactGroup) {
        let hits = (ok_ns.len() + err_ns.len()) as u64;
        let to_f64s = |xs: &[u64]| -> Vec<f64> { xs.iter().map(|&x| x as f64).collect() };
        let mut g = pb::ClientGroupedStats {
            service: "svc".into(),
            name: "test.op".into(),
            resource: "GET /foo".into(),
            r#type: "web".into(),
            hits,
            errors: err_ns.len() as u64,
            top_level_hits: hits,
            ok_summary: if ok_ns.is_empty() {
                Vec::new()
            } else {
                sketch(&to_f64s(ok_ns))
            },
            error_summary: if err_ns.is_empty() {
                Vec::new()
            } else {
                sketch(&to_f64s(err_ns))
            },
            ..Default::default()
        };
        customize(&mut g);
        (
            g,
            OtlpExactGroup {
                ok: cell(ok_ns),
                error: cell(err_ns),
            },
        )
    }

    fn buckets(groups: Vec<(pb::ClientGroupedStats, OtlpExactGroup)>) -> Vec<OtlpStatsBucket> {
        let (stats, exact): (Vec<_>, Vec<_>) = groups.into_iter().unzip();
        vec![OtlpStatsBucket {
            bucket: pb::ClientStatsBucket {
                start: T_START,
                duration: T_SIZE,
                stats,
                agent_time_shift: 0,
            },
            exact,
        }]
    }

    /// Single-cell default group with a 1s ok span.
    fn one_ok_group() -> (pb::ClientGroupedStats, OtlpExactGroup) {
        group_with_exact(&[1_000_000_000], &[], |_| {})
    }

    fn metric(req: &Value) -> &Value {
        &req["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][0]
    }

    fn points(req: &Value) -> &Vec<Value> {
        metric(req)["histogram"]["dataPoints"].as_array().unwrap()
    }

    fn resource_attrs(req: &Value) -> &Vec<Value> {
        req["resourceMetrics"][0]["resource"]["attributes"]
            .as_array()
            .unwrap()
    }

    fn str_at<'a>(attrs: &'a [Value], key: &str) -> Option<&'a str> {
        attrs
            .iter()
            .find(|kv| kv["key"] == key)
            .and_then(|kv| kv["value"]["stringValue"].as_str())
    }

    fn err_code() -> String {
        (status_code::ERROR as i64).to_string()
    }

    fn is_error_point(p: &Value) -> bool {
        let ec = err_code();
        p["attributes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|kv| kv["key"] == "status.code" && kv["value"]["intValue"].as_str() == Some(&ec))
    }

    #[test]
    fn metric_shape_and_resource_attributes() {
        assert!(map_stats_to_otlp_metrics(&[], &resource(), false).is_none());
        let mut r = resource();
        r.app_version = "1.2.3".to_string();
        r.hostname = "my-host".to_string();
        r.runtime_id = "abc-123".to_string();
        r.process_tags = "entrypoint.name:server".to_string();
        for otel in [false, true] {
            let req = map_stats_to_otlp_metrics(&buckets(vec![one_ok_group()]), &r, otel).unwrap();
            let m = metric(&req);
            assert_eq!(m["name"], "traces.span.sdk.metrics.duration");
            assert_eq!(m["unit"], "s");
            assert_eq!(m["histogram"]["aggregationTemporality"], 1);
            assert!(req["resourceMetrics"][0]["scopeMetrics"][0]["scope"].is_null());
            let a = resource_attrs(&req);
            assert_eq!(str_at(a, "service.name"), Some("svc"));
            assert_eq!(str_at(a, "service.version"), Some("1.2.3"));
            assert_eq!(str_at(a, "deployment.environment.name"), Some("test"));
            assert_eq!(str_at(a, "host.name"), Some("my-host"));
            assert_eq!(str_at(a, "telemetry.sdk.name"), Some("datadog"));
            let dd = !otel;
            assert_eq!(str_at(a, "datadog.runtime_id").is_some(), dd);
            assert_eq!(str_at(a, "datadog.entrypoint.name").is_some(), dd);
        }
    }

    #[test]
    fn data_point_attributes_and_otel_strip() {
        let g_pair = group_with_exact(&[1_000_000_000], &[], |g| {
            g.http_status_code = 404;
            g.http_method = "POST".into();
            g.http_endpoint = "/users/:id".into();
            g.synthetics = true;
        });
        let custom_pair = group_with_exact(&[1_000_000_000], &[], |g| {
            g.service = "svc-other".into();
        });
        let bs = buckets(vec![g_pair.clone(), custom_pair]);

        let req = map_stats_to_otlp_metrics(&bs, &resource(), false).unwrap();
        let pts = points(&req);
        let a = pts
            .iter()
            .find(|p| str_at(p["attributes"].as_array().unwrap(), "service.name").is_none())
            .unwrap()["attributes"]
            .as_array()
            .unwrap();
        assert_eq!(str_at(a, "span.name"), Some("GET /foo"));
        assert_eq!(str_at(a, "http.request.method"), Some("POST"));
        assert_eq!(str_at(a, "http.route"), Some("/users/:id"));
        assert!(a.iter().any(|kv| kv["key"] == "http.response.status_code"));
        assert_eq!(str_at(a, "datadog.operation.name"), Some("test.op"));
        assert_eq!(str_at(a, "datadog.span.type"), Some("web"));
        assert_eq!(str_at(a, "datadog.origin"), Some("synthetics"));
        assert!(a.iter().any(|kv| kv["key"] == "datadog.span.top_level"));
        assert!(pts.iter().any(
            |p| str_at(p["attributes"].as_array().unwrap(), "service.name") == Some("svc-other")
        ));

        // OTel mode strips datadog.*/_datadog.* attributes.
        let req = map_stats_to_otlp_metrics(&buckets(vec![g_pair]), &resource(), true).unwrap();
        let a = points(&req)[0]["attributes"].as_array().unwrap();
        assert!(!a.iter().any(|kv| {
            let k = kv["key"].as_str().unwrap_or("");
            k.starts_with("datadog.") || k.starts_with("_datadog.")
        }));
        assert_eq!(str_at(a, "http.request.method"), Some("POST"));
    }

    #[test]
    fn emits_canonical_grpc_status_name_for_rpc_response_status_code() {
        let g = group_with_exact(&[1_000_000_000], &[], |g| {
            g.grpc_status_code = "5".into();
        });
        let req = map_stats_to_otlp_metrics(&buckets(vec![g]), &resource(), false).unwrap();
        let a = points(&req)[0]["attributes"].as_array().unwrap();
        assert_eq!(str_at(a, "rpc.response.status_code"), Some("NOT_FOUND"));

        // Empty/unmapped codes omit the attribute.
        for code in ["", "99"] {
            let g = group_with_exact(&[1_000_000_000], &[], |g| {
                g.grpc_status_code = code.into();
            });
            let req = map_stats_to_otlp_metrics(&buckets(vec![g]), &resource(), false).unwrap();
            let a = points(&req)[0]["attributes"].as_array().unwrap();
            assert!(!a.iter().any(|kv| kv["key"] == "rpc.response.status_code"));
        }
    }

    #[test]
    fn histogram_values_are_exact_and_distribution_uses_sketch() {
        // Single 1s ok span: count/sum/min/max all exact, distribution shaped by the sketch.
        let req =
            map_stats_to_otlp_metrics(&buckets(vec![one_ok_group()]), &resource(), false).unwrap();
        let p = &points(&req)[0];
        assert_eq!(p["count"], "1");
        assert_eq!(p["sum"].as_f64().unwrap(), 1.0);
        assert_eq!(p["min"].as_f64().unwrap(), 1.0);
        assert_eq!(p["max"].as_f64().unwrap(), 1.0);
        assert_eq!(p["startTimeUnixNano"], T_START.to_string());
        assert_eq!(p["timeUnixNano"], (T_START + T_SIZE).to_string());
        assert_eq!(p["explicitBounds"], json!(EXPLICIT_BOUNDS_SECONDS));
        assert_eq!(
            p["bucketCounts"].as_array().unwrap().len(),
            EXPLICIT_BOUNDS_SECONDS.len() + 1
        );

        // 3ms, 300ms, 3s land in three distinct buckets; exact sum = 3.303s.
        let g = group_with_exact(&[3_000_000, 300_000_000, 3_000_000_000], &[], |_| {});
        let req = map_stats_to_otlp_metrics(&buckets(vec![g]), &resource(), false).unwrap();
        let p = &points(&req)[0];
        assert_eq!(p["count"], "3");
        assert_eq!(p["sum"].as_f64().unwrap(), ns_to_s(3_303_000_000));
        assert_eq!(p["min"].as_f64().unwrap(), ns_to_s(3_000_000));
        assert_eq!(p["max"].as_f64().unwrap(), ns_to_s(3_000_000_000));
        let counts = p["bucketCounts"].as_array().unwrap();
        assert_eq!(counts.iter().filter(|c| c.as_str() != Some("0")).count(), 3);
    }

    #[test]
    fn mixed_ok_and_error_have_exact_independent_sums() {
        // 2 ok spans + 1 error span. ok_sum + error_sum must equal the combined group duration.
        let ok = [200_000_000_u64, 300_000_000];
        let err = [700_000_000_u64];
        let combined_ns = ok.iter().sum::<u64>() + err.iter().sum::<u64>();
        let g = group_with_exact(&ok, &err, |_| {});
        let req = map_stats_to_otlp_metrics(&buckets(vec![g]), &resource(), false).unwrap();
        let pts = points(&req);
        assert_eq!(pts.len(), 2);
        let ok_pt = pts.iter().find(|p| !is_error_point(p)).unwrap();
        let err_pt = pts.iter().find(|p| is_error_point(p)).unwrap();

        // Each cell's sum is exact and independent of the other.
        assert_eq!(ok_pt["count"], "2");
        assert_eq!(ok_pt["sum"].as_f64().unwrap(), ns_to_s(500_000_000));
        assert_eq!(ok_pt["min"].as_f64().unwrap(), ns_to_s(200_000_000));
        assert_eq!(ok_pt["max"].as_f64().unwrap(), ns_to_s(300_000_000));
        assert_eq!(err_pt["count"], "1");
        assert_eq!(err_pt["sum"].as_f64().unwrap(), ns_to_s(700_000_000));
        assert_eq!(err_pt["min"].as_f64().unwrap(), ns_to_s(700_000_000));
        assert_eq!(err_pt["max"].as_f64().unwrap(), ns_to_s(700_000_000));

        // ok_sum + error_sum equals the combined group duration.
        let ok_s = ok_pt["sum"].as_f64().unwrap();
        let err_s = err_pt["sum"].as_f64().unwrap();
        assert_eq!(ok_s + err_s, ns_to_s(combined_ns));
    }

    #[test]
    fn test_grpc_status_code_to_name() {
        assert_eq!(grpc_status_code_to_name("0"), Some("OK"));
        assert_eq!(grpc_status_code_to_name("5"), Some("NOT_FOUND"));
        assert_eq!(grpc_status_code_to_name("14"), Some("UNAVAILABLE"));
        assert_eq!(grpc_status_code_to_name("16"), Some("UNAUTHENTICATED"));
        assert_eq!(grpc_status_code_to_name("17"), None);
        assert_eq!(grpc_status_code_to_name(""), None);
        assert_eq!(grpc_status_code_to_name("OK"), None);
    }
}
