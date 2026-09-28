// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Privacy operations shared by aggregation and the independent output guard.

use sha2::{Digest, Sha256};

pub(super) fn protected_targeting_key(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    format!("sha256_{:x}", Sha256::digest(value.as_bytes()))
}

/// OpenFeature error codes only: evaluator text can contain customer data even
/// when the customer consented to collecting evaluation context.
pub(super) fn error_code(value: &str) -> &'static str {
    match value {
        "PROVIDER_NOT_READY" => "PROVIDER_NOT_READY",
        "PROVIDER_FATAL" => "PROVIDER_FATAL",
        "FLAG_NOT_FOUND" => "FLAG_NOT_FOUND",
        "PARSE_ERROR" => "PARSE_ERROR",
        "TYPE_MISMATCH" => "TYPE_MISMATCH",
        "TARGETING_KEY_MISSING" => "TARGETING_KEY_MISSING",
        "INVALID_CONTEXT" => "INVALID_CONTEXT",
        _ => "GENERAL",
    }
}
