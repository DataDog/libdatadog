// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// Higher-order trace ID bits propagation tag.
#[allow(unused)]
pub const HIGHER_ORDER_TRACE_ID_BITS_TAG: &str = "_dd.p.tid";

/// Span kind meta tag.
#[allow(unused)]
pub const SPAN_KIND_TAG: &str = "span.kind";

/// Event extraction sampling rate metric key.
pub const SAMPLING_RATE_EVENT_EXTRACTION_KEY: &str = "_dd1.sr.eausr";

/// Sampling priority metric key.
pub const SAMPLING_PRIORITY_TAG_KEY: &str = "_sampling_priority_v1";

/// Sampling decision maker propagation tag key.
pub const SAMPLING_DECISION_MAKER_TAG_KEY: &str = "_dd.p.dm";

/// Sampling rule rate metric key.
pub const SAMPLING_RULE_RATE_TAG_KEY: &str = "_dd.rule_psr";

/// Sampling agent rate metric key.
pub const SAMPLING_AGENT_RATE_TAG_KEY: &str = "_dd.agent_psr";

/// Rate limiter effective rate metric key.
pub const RL_EFFECTIVE_RATE: &str = "_dd.limit_psr";

/// Knuth Sampling Rate propagated tag key.
pub const SAMPLING_KNUTH_RATE_TAG_KEY: &str = "_dd.p.ksr";
