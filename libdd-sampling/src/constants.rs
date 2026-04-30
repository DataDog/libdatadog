// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared constants for the datadog_opentelemetry::sampling crate

/// Sampling rate limits
pub mod rate {
    /// Maximum sampling rate
    pub const MAX_SAMPLE_RATE: f64 = 1.0;
    /// Minimum sampling rate
    pub const MIN_SAMPLE_RATE: f64 = 0.0;
}

/// Pattern matching constants
pub mod pattern {
    /// Marker to represent "no rule" for a field (empty string)
    pub const NO_RULE: &str = "";
}

/// Numeric constants used in sampling algorithms
pub mod numeric {
    /// Knuth's multiplicative hash factor for deterministic sampling
    pub const KNUTH_FACTOR: u64 = 1_111_111_111_111_111_111;
    /// Maximum 64-bit unsigned integer value
    pub const MAX_UINT_64BITS: u64 = u64::MAX;
}

#[allow(unused)]
/// Attribute keys used in tracing
pub mod attr {
    /// Service name attribute key
    pub const SERVICE_TAG: &str = "service.name";
    /// Environment attribute key
    pub const ENV_TAG: &str = "env";
    /// Resource name attribute key
    pub const RESOURCE_TAG: &str = "resource.name";
}

#[allow(unused)]
/// Rule provenance categories
pub mod provenance {
    /// Customer-defined rules
    pub const CUSTOMER: &str = "customer";
    /// Dynamically loaded rules
    pub const DYNAMIC: &str = "dynamic";
    /// Default built-in rules
    pub const DEFAULT: &str = "default";
}
