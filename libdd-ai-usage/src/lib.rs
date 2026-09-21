// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Content-free accounting for **one completed AI request**, not cumulative session totals.
//!
//! Adapters select fields from their SDK and declare whether input includes caches.
//! This crate partitions reported usage without tokenization, pricing, or guessing missing
//! values. Observations can overlap; only `quantities` are suitable for additive accounting.
//! It performs no I/O, reads no configuration, and owns no background work.

use std::collections::{BTreeMap, BTreeSet};

/// Largest integer exactly representable by downstream floating-point metric transports.
pub const MAX_COUNT: u64 = 1 << 53;

/// A reported scalar. `Missing` differs from an explicitly reported zero.
/// Adapters must use `Invalid` for wrong types (including booleans), negative integers,
/// or integers that cannot fit in a `u64`. Fractions are accepted only for durations.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Value {
    #[default]
    Missing,
    Integer(u64),
    Fraction(f64),
    Invalid,
}

impl Value {
    fn count(self) -> Result<Option<u64>, Issue> {
        match self {
            Self::Missing => Ok(None),
            Self::Integer(value) if value <= MAX_COUNT => Ok(Some(value)),
            _ => Err(Issue::InvalidUsage),
        }
    }

    fn duration(self) -> Result<Option<Measurement>, Issue> {
        match self {
            Self::Missing => Ok(None),
            Self::Integer(value) if value <= MAX_COUNT => Ok(Some(Measurement::Count(value))),
            // MAX_COUNT is exactly representable as f64.
            Self::Fraction(value)
                if value.is_finite() && (0.0..=MAX_COUNT as f64).contains(&value) =>
            {
                Ok(Some(Measurement::Duration(value)))
            }
            _ => Err(Issue::InvalidUsage),
        }
    }
}

/// Whether the reported input total already includes cache reads and writes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputBasis {
    #[default]
    IncludesCache,
    /// For example, native Anthropic Messages input (not LiteLLM-normalized prompt usage).
    ExcludesCache,
}

/// Reported details are observations, not additional tokens to add to a total.
#[derive(Clone, Debug, Default)]
pub struct Details {
    pub text_tokens: Value,
    pub audio_tokens: Value,
    pub image_tokens: Value,
    pub video_tokens: Value,
    pub cached_tokens: Value,
    pub reasoning_tokens: Value,
    pub tool_use_tokens: Value,
    pub character_count: Value,
    pub image_count: Value,
    pub accepted_prediction_tokens: Value,
    pub rejected_prediction_tokens: Value,
    pub audio_length_seconds: Value,
    pub video_length_seconds: Value,
}

impl Details {
    fn counts(&self) -> [(&'static str, Value); 11] {
        [
            ("text_tokens", self.text_tokens),
            ("audio_tokens", self.audio_tokens),
            ("image_tokens", self.image_tokens),
            ("video_tokens", self.video_tokens),
            ("cached_tokens", self.cached_tokens),
            ("reasoning_tokens", self.reasoning_tokens),
            ("tool_use_tokens", self.tool_use_tokens),
            ("character_count", self.character_count),
            ("image_count", self.image_count),
            (
                "accepted_prediction_tokens",
                self.accepted_prediction_tokens,
            ),
            (
                "rejected_prediction_tokens",
                self.rejected_prediction_tokens,
            ),
        ]
    }
}

/// Both native and SDK-normalized tool counts, when both were reported.
/// Equal counts are counted once; a disagreement suppresses tool quantities.
#[derive(Clone, Copy, Debug, Default)]
pub struct ToolCount {
    pub native: Value,
    pub normalized: Value,
}

/// Safe, selected fields from one response. No prompts, credentials, or arbitrary metadata.
/// SDK aliases and authenticated identity selection belong in the caller's adapter.
#[derive(Clone, Debug, Default)]
pub struct UsageInput {
    pub input: Value,
    pub output: Value,
    pub input_basis: InputBasis,
    /// Prompt-only embedding responses need not report output or cache detail.
    pub embedding: bool,
    pub cache_read: Value,
    pub cache_write: Value,
    pub cache_write_5m: Value,
    pub cache_write_1h: Value,
    pub input_details: Details,
    pub output_details: Details,
    pub web_search_requests: ToolCount,
    pub tool_search_requests: ToolCount,
    pub browser_open_requests: ToolCount,
    pub google_maps_grounding_requests: ToolCount,
}

/// Integer counters retain their type, including at the maximum supported count.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Measurement {
    Count(u64),
    Duration(f64),
}

impl Measurement {
    fn nonzero(self) -> bool {
        match self {
            Self::Count(value) => value != 0,
            Self::Duration(value) => value != 0.0,
        }
    }
}

/// Machine-readable reasons why all or part of a request cannot be partitioned safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Issue {
    MissingUsage,
    InvalidUsage,
    UnsupportedUsageShape,
    InconsistentUsage,
    InconsistentCacheTtl,
    CacheReadDetailMissing,
    CacheWriteDetailMissing,
    CacheWriteTtlUnknown,
    MultimodalPartitionUnsupported,
    ConflictingToolUsage,
}

impl Issue {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingUsage => "missing_usage",
            Self::InvalidUsage => "invalid_usage",
            Self::UnsupportedUsageShape => "unsupported_usage_shape",
            Self::InconsistentUsage => "inconsistent_usage",
            Self::InconsistentCacheTtl => "inconsistent_cache_ttl",
            Self::CacheReadDetailMissing => "cache_read_detail_missing",
            Self::CacheWriteDetailMissing => "cache_write_detail_missing",
            Self::CacheWriteTtlUnknown => "cache_write_ttl_unknown",
            Self::MultimodalPartitionUnsupported => "multimodal_partition_unsupported",
            Self::ConflictingToolUsage => "conflicting_tool_usage",
        }
    }
}

/// Disjoint quantities and potentially overlapping observations. Units are in the names.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub quantities: BTreeMap<&'static str, u64>,
    pub observations: BTreeMap<String, Measurement>,
    pub issues: BTreeSet<Issue>,
}

impl Usage {
    fn observe(&mut self, key: &str, value: u64) {
        self.observations
            .insert(key.into(), Measurement::Count(value));
    }
}

/// Normalize one response. Missing, invalid, or ambiguous data never invents usage.
/// Incomplete cache information retains known quantities but not inferred uncached input.
///
/// ```
/// use libdd_ai_usage::{normalize, UsageInput, Value};
/// let usage = normalize(Some(&UsageInput {
///     input: Value::Integer(100),
///     output: Value::Integer(10),
///     cache_read: Value::Integer(40),
///     cache_write: Value::Integer(0),
///     ..Default::default()
/// }));
/// assert_eq!(usage.quantities["input_uncached_tokens"], 60);
/// assert_eq!(usage.quantities["output_tokens"], 10);
/// ```
pub fn normalize(input: Option<&UsageInput>) -> Usage {
    let mut result = Usage::default();
    match input {
        None => {
            result.issues.insert(Issue::MissingUsage);
        }
        Some(input) => {
            if let Err(issue) = partition(input, &mut result) {
                result.quantities.clear();
                result.issues.insert(issue);
            }
        }
    }
    result
}

fn partition(input: &UsageInput, result: &mut Usage) -> Result<(), Issue> {
    let prompt = input.input.count()?;
    let output = input.output.count()?;
    for (prefix, details) in [
        ("input", &input.input_details),
        ("output", &input.output_details),
    ] {
        for (name, value) in details.counts() {
            if let Some(value) = value.count()? {
                result.observe(&format!("{prefix}_{name}"), value);
            }
        }
        for (name, value) in [
            ("audio_length_seconds", details.audio_length_seconds),
            ("video_length_seconds", details.video_length_seconds),
        ] {
            if let Some(value) = value.duration()? {
                result
                    .observations
                    .insert(format!("{prefix}_{name}"), value);
            }
        }
    }
    let tools = [
        ("web_search_requests", input.web_search_requests),
        ("tool_search_requests", input.tool_search_requests),
        ("browser_open_requests", input.browser_open_requests),
        (
            "google_maps_grounding_requests",
            input.google_maps_grounding_requests,
        ),
    ];
    for (name, tool) in tools {
        let native = tool.native.count()?;
        let normalized = tool.normalized.count()?;
        if native.is_some() && normalized.is_some() && native != normalized {
            result.issues.insert(Issue::ConflictingToolUsage);
        }
        if let Some(value) = native.or(normalized) {
            result.observe(name, value);
        }
    }
    let Some(mut prompt) = prompt.filter(|_| output.is_some() || input.embedding) else {
        return Err(Issue::UnsupportedUsageShape);
    };
    let cached = input.cache_read.count()?;
    let written = input.cache_write.count()?;
    if input.input_basis == InputBasis::ExcludesCache {
        // Each term is bounded by MAX_COUNT, so the sum cannot overflow u64.
        prompt += cached.unwrap_or_default() + written.unwrap_or_default();
        if prompt > MAX_COUNT {
            return Err(Issue::InvalidUsage);
        }
    }
    result.observe("input_tokens", prompt);
    result.observe("context_tokens", prompt);
    if let Some(output) = output {
        result.observe("output_tokens", output);
    }
    result.observe("input_cache_read_reported", u64::from(cached.is_some()));
    result.observe("input_cache_write_reported", u64::from(written.is_some()));
    for (name, value, issue) in [
        (
            "input_cache_read_tokens",
            cached,
            Issue::CacheReadDetailMissing,
        ),
        (
            "input_cache_write_tokens",
            written,
            Issue::CacheWriteDetailMissing,
        ),
    ] {
        if let Some(value) = value {
            result.observe(name, value);
        } else if !input.embedding {
            result.issues.insert(issue);
        }
    }
    let cache_complete = input.embedding || (cached.is_some() && written.is_some());
    let read = cached.unwrap_or_default();
    let write = written.unwrap_or_default();
    let audio_out = input
        .output_details
        .audio_tokens
        .count()?
        .unwrap_or_default();
    let reasoning = input
        .output_details
        .reasoning_tokens
        .count()?
        .unwrap_or_default();
    let five = input.cache_write_5m.count()?.unwrap_or_default();
    let hour = input.cache_write_1h.count()?.unwrap_or_default();
    if written.is_some() && five + hour > write {
        return Err(Issue::InconsistentCacheTtl);
    }
    result.observe("input_cache_write_5m_tokens", five);
    result.observe("input_cache_write_1h_tokens", hour);
    if read + write > prompt
        || audio_out > output.unwrap_or_default()
        || reasoning > output.unwrap_or_default()
    {
        return Err(Issue::InconsistentUsage);
    }
    for prefix in ["input", "output"] {
        for key in [
            "audio_tokens",
            "image_tokens",
            "video_tokens",
            "image_count",
            "audio_length_seconds",
            "video_length_seconds",
        ] {
            if result
                .observations
                .get(&format!("{prefix}_{key}"))
                .is_some_and(|value| value.nonzero())
            {
                result.issues.insert(Issue::MultimodalPartitionUnsupported);
                return Ok(());
            }
        }
    }
    if cache_complete {
        result
            .quantities
            .insert("input_uncached_tokens", prompt - read - write);
    }
    if cached.is_some() {
        result.quantities.insert("input_cache_read_tokens", read);
    }
    if let Some(output) = output {
        result.quantities.insert("output_tokens", output);
    }
    result.observe("reasoning_output_tokens", reasoning);
    if written.is_some() {
        result
            .quantities
            .insert("input_cache_write_5m_tokens", five);
        result
            .quantities
            .insert("input_cache_write_1h_tokens", hour);
        result
            .quantities
            .insert("input_cache_write_unknown_ttl_tokens", write - five - hour);
    }
    if write > five + hour {
        result.issues.insert(Issue::CacheWriteTtlUnknown);
    }
    if !result.issues.contains(&Issue::ConflictingToolUsage) {
        for (name, _) in tools {
            if let Some(Measurement::Count(value)) = result.observations.get(name) {
                result.quantities.insert(name, *value);
            }
        }
    }
    Ok(())
}

/// Provider-independent decimal-k boundaries: 32k doubling forever, plus 200k and 272k.
/// Pass `reliable = false` when contradictory usage makes a reported context untrustworthy.
/// No model catalog is embedded; callers decide how a bucket affects pricing.
pub fn context_tokens_bucket(tokens: Value, reliable: bool) -> String {
    let Ok(Some(tokens)) = tokens.count() else {
        return "unknown".into();
    };
    if !reliable {
        return "unknown".into();
    }
    let (mut lower, mut upper) = (0, 32_000);
    // MAX_COUNT bounds the largest upper at 32_000 * 2^39, safely inside u64.
    while tokens > upper {
        lower = upper + 1;
        upper *= 2;
    }
    for boundary in [200_000, 272_000] {
        if tokens <= boundary {
            upper = upper.min(boundary);
        } else {
            lower = lower.max(boundary + 1);
        }
    }
    format!("{lower}_{upper}")
}

/// Basic validation for a label already selected by a trusted adapter.
/// This is NOT a generic secret detector: never pass arbitrary headers or credentials.
/// Length is measured in Unicode scalar values, matching Python's string length.
pub fn valid_label(value: &str, max_length: usize) -> bool {
    !value.is_empty()
        && value.chars().take(max_length.saturating_add(1)).count() <= max_length
        && value.trim_matches(python_whitespace) == value
        && !value.chars().any(|c| c.is_ascii_control())
        && !value
            .get(..3)
            .is_some_and(|s| s.eq_ignore_ascii_case("sk-"))
        && !value
            .get(..7)
            .is_some_and(|s| s.eq_ignore_ascii_case("bearer "))
}

fn python_whitespace(c: char) -> bool {
    // Python additionally considers the ASCII information separators whitespace.
    c.is_whitespace() || matches!(c, '\u{1c}'..='\u{1f}')
}
