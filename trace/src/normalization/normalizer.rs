// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache License Version 2.0. This product includes software
// developed at Datadog (https://www.datadoghq.com/). Copyright 2023-Present
// Datadog, Inc.

use std::time::{SystemTime};
use crate::errors;
use crate::normalize_utils;
use crate::pb;

const MAX_TYPE_LEN: i64 = 100;

// an arbitrary cutoff to spot weird-looking values
// nanoseconds since epoch on Jan 1, 2000
const YEAR_2000_NANOSEC_TS: i64 = 946684800000000000;

fn normalize(s: &mut pb::Span) -> Result<(), errors::NormalizerError> {
    if s.trace_id == 0 {
        return Err(errors::NormalizerError::new("TraceID is zero (reason:trace_id_zero)"));
    }
    if s.span_id == 0 {
        return Err(errors::NormalizerError::new("SpanID is zero (reason:span_id_zero)"));
    }
    
    // TODO: The second parameter of normalize::normalize_service should be language
    // let (svc, err) = normalize_utils::normalize_service(s.service.clone(), "".to_string());
    // match err {
    //     Some(errors::NormalizeErrors::ErrorEmpty) => println!("Fixing malformed trace. Service is empty (reason:service_empty)"),
    //     Some(errors::NormalizeErrors::ErrorTooLong) => println!("Fixing malformed trace. Service is too long (reason:service_truncate)"),
    //     Some(errors::NormalizeErrors::ErrorInvalid) => println!("Fixing malformed trace. Service is invalid (reason:service_invalid)"),
    //     None => ()
    // }
    // s.service = svc;

    // TODO: check for a feature flag to determine the component tag to become the span name
    // https://github.com/DataDog/datadog-agent/blob/dc88d14851354cada1d15265220a39dce8840dcc/pkg/trace/agent/normalizer.go#L64

    let (normalized_name, err) = normalize_utils::normalize_name(s.name.clone());
    match err {
        Some(errors::NormalizeErrors::ErrorEmpty) => println!("Fixing malformed trace. Name is empty (reason:span_name_empty)"),
        Some(errors::NormalizeErrors::ErrorTooLong) => println!("Fixing malformed trace. Name is too long (reason:span_name_truncate)"),
        Some(errors::NormalizeErrors::ErrorInvalid) => println!("Fixing malformed trace. Name is invalid (reason:span_name_invalid)"),
        None => ()
    }
    s.name = normalized_name;

    if s.resource.is_empty() {
        println!("Fixing malformed trace. Resource is empty (reason:resource_empty)");
        s.resource = s.name.clone();
    }

    // ParentID, TraceID and SpanID set in the client could be the same
	// Supporting the ParentID == TraceID == SpanID for the root span, is compliant
	// with the Zipkin implementation. Furthermore, as described in the PR
	// https://github.com/openzipkin/zipkin/pull/851 the constraint that the
	// root span's ``trace id = span id`` has been removed
	if s.parent_id == s.trace_id && s.parent_id == s.span_id {
		s.parent_id = 0;
		println!("span.normalize: `ParentID`, `TraceID` and `SpanID` are the same; `ParentID` set to 0");
	}

    // Start & Duration as nanoseconds timestamps
	// if s.Start is very little, less than year 2000 probably a unit issue so discard
	// (or it is "le bug de l'an 2000")
    if s.duration < 0 {
        println!("Fixing malformed trace. Duration is invalid (reason:invalid_duration), setting span.duration=0");
        s.duration = 0;
    }
    if s.duration > std::i64::MAX-s.start {
        println!("Fixing malformed trace. Duration is too large and causes overflow (reason:invalid_duration), setting span.duration=0");
        s.duration = 0;
    }
    if s.start < YEAR_2000_NANOSEC_TS {
        println!("Fixing malformed trace. Start date is invalid (reason:invalid_start_date), setting span.start=time.now()");
        let now: i64 = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos() as i64;
        s.start = now - s.duration;
        if s.start < 0 {
            s.start = now;
        }
    }

    if s.r#type.len() > MAX_TYPE_LEN as usize {
        println!("Fixing malformed trace. Type is too long (reason:type_truncate), truncating span.type to length={}", MAX_TYPE_LEN);
        s.r#type = normalize_utils::truncate_utf8(s.r#type.clone(), MAX_TYPE_LEN);
    }

    // TODO: Implement tag normalization
    // if s.meta.contains_key("env") {
    //     let env_tag: String = s.meta.get("env").unwrap().to_string();
    //     s.meta.insert("env".to_string(), normalize_utils::normalize_tag(env_tag));
    // }
    if s.meta.contains_key("http.status_code") {
        let status_code: String = s.meta.get("http.status_code").unwrap().to_string();
        if !is_valid_status_code(status_code.clone()) {
            println!("Fixing malformed trace. HTTP status code is invalid (reason:invalid_http_status_code), dropping invalid http.status_code={}", status_code);
            s.meta.remove("http.status_code");
        }
    }

    Ok(())
}

pub fn is_valid_status_code(sc: String) -> bool {
    match sc.parse::<i64>() {
        Ok(code) => {
            (100..600).contains(&code)
        },
        Err(..) => {
            false
        }
    }
}
