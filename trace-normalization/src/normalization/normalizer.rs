// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use std::time::{SystemTime};
use crate::errors;
use crate::normalize_utils;
use crate::pb;

const MAX_TYPE_LEN: usize = 100;

// an arbitrary cutoff to spot weird-looking values
// nanoseconds since epoch on Jan 1, 2000
const YEAR_2000_NANOSEC_TS: i64 = 946684800000000000;

// DEFAULT_SPAN_NAME is the default name we assign a span if it's missing and we have no reasonable fallback
pub const DEFAULT_SPAN_NAME: &str = "unnamed_operation";

#[allow(dead_code)]
pub fn normalize(s: &mut pb::Span) -> Result<(), errors::NormalizerError> {
    if s.trace_id == 0 {
        return Err(errors::NormalizerError::new("TraceID is zero (reason:trace_id_zero)"));
    }
    if s.span_id == 0 {
        return Err(errors::NormalizerError::new("SpanID is zero (reason:span_id_zero)"));
    }
    
    // TODO: Implement service name normalizer in future PR
    // let (svc, _) = normalize_utils::normalize_service(s.service.clone(), "".to_string());
    // s.service = svc;

    // TODO: check for a feature flag to determine the component tag to become the span name
    // https://github.com/DataDog/datadog-agent/blob/dc88d14851354cada1d15265220a39dce8840dcc/pkg/trace/agent/normalizer.go#L64

    let normalized_name = match normalize_utils::normalize_name(s.name.clone()) {
        Ok(name) => name,
        Err(_) => {
            DEFAULT_SPAN_NAME.to_string()
        }
    };

    s.name = normalized_name;

    if s.resource.is_empty() {
        s.resource = s.name.clone();
    }

    // ParentID, TraceID and SpanID set in the client could be the same
    // Supporting the ParentID == TraceID == SpanID for the root span, is compliant
    // with the Zipkin implementation. Furthermore, as described in the PR
    // https://github.com/openzipkin/zipkin/pull/851 the constraint that the
    // root span's ``trace id = span id`` has been removed
    if s.parent_id == s.trace_id && s.parent_id == s.span_id {
        s.parent_id = 0;
    }

    // Start & Duration as nanoseconds timestamps
	// if s.Start is very little, less than year 2000 probably a unit issue so discard
	// (or it is "le bug de l'an 2000")
    if s.duration < 0 {
        s.duration = 0;
    }
    if s.duration > std::i64::MAX-s.start {
        s.duration = 0;
    }
    if s.start < YEAR_2000_NANOSEC_TS {
        let now: i64 = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos() as i64;
        s.start = now - s.duration;
        if s.start < 0 {
            s.start = now;
        }
    }

    if s.r#type.len() > MAX_TYPE_LEN {
        s.r#type = normalize_utils::truncate_utf8(s.r#type.clone(), MAX_TYPE_LEN);
    }

    // TODO: Implement tag normalization in future PR
    // if s.meta.contains_key("env") {
    //     let env_tag: String = s.meta.get("env").unwrap().to_string();
    //     s.meta.insert("env".to_string(), normalize_utils::normalize_tag(env_tag));
    // }

    if s.meta.contains_key("http.status_code") {
        let status_code: String = s.meta.get("http.status_code").unwrap().to_string();
        if !is_valid_status_code(status_code) {
            s.meta.remove("http.status_code");
        }
    }

    Ok(())
}

pub fn is_valid_status_code(sc: String) -> bool {
    if let Ok(code) = sc.parse::<i64>() {
        return (100..600).contains(&code);
    }
    false
}
