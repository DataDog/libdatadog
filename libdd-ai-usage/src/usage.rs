// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Usage normalization (`PORTABLE-METRICS.md#usage-normalization`): one
//! procedure for the provider-attempt and token-breakdown profiles. It
//! records what is known, names what is not with an issue code, and rejects
//! only a malformed observation.

use std::collections::BTreeMap;

use crate::number::{CountReading, MAX_SAFE_INTEGER, read_count};
use crate::point::{ErrorCode, IssueCode, Issues, MetricError, reject};

/// Whether the reported `input_tokens` counts cached tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputBasis {
    IncludesCache,
    ExcludesCache,
}

/// Whether the reported `output_tokens` counts reasoning tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputBasis {
    IncludesReasoning,
    ExcludesReasoning,
}

/// A `gen_ai.token.modality` value. The derived order is the recording
/// order: the three reported kinds, then the remainder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Modality {
    Text,
    Image,
    Audio,
    Unknown,
}

impl Modality {
    /// The modalities an observation can report, in check and record order.
    pub const REPORTED: [Modality; 3] = [Modality::Text, Modality::Image, Modality::Audio];

    pub fn as_str(self) -> &'static str {
        match self {
            Modality::Text => "text",
            Modality::Image => "image",
            Modality::Audio => "audio",
            Modality::Unknown => "unknown",
        }
    }
}

/// The usage an observation reports, as read: numbers are not yet checked,
/// and a basis is still a string. `None` is "not reported"; a JSON `null`
/// reads as `None`. An infinite number stands for a token no double holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReportedUsage {
    pub input_tokens: Option<f64>,
    pub output_tokens: Option<f64>,
    pub cache_read_input_tokens: Option<f64>,
    pub cache_write_input_tokens: Option<f64>,
    pub cache_write_5m_input_tokens: Option<f64>,
    pub cache_write_1h_input_tokens: Option<f64>,
    pub reasoning_output_tokens: Option<f64>,
    pub web_search_requests: Option<f64>,
    pub web_search_requests_secondary: Option<f64>,
    pub input_basis: Option<String>,
    pub output_basis: Option<String>,
    /// Parts by modality key. A `null` part is left out when reading.
    pub input_tokens_by_modality: Option<BTreeMap<String, f64>>,
    pub output_tokens_by_modality: Option<BTreeMap<String, f64>>,
}

/// What normalization found. A `None` was not reported, was dropped by the
/// bound, could not be formed, or belongs to a group that was withheld; the
/// issue codes say which.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NormalizedUsage {
    /// The inclusive input: cached tokens counted in.
    pub input_total: Option<u64>,
    /// The inclusive output: reasoning counted in.
    pub output_total: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write_5m: Option<u64>,
    pub cache_write_1h: Option<u64>,
    pub cache_write_unspecified: Option<u64>,
    pub uncached_input: Option<u64>,
    pub reasoning: Option<u64>,
    /// Increments of the input Counter, with the `unknown` remainder last.
    pub input_by_modality: Vec<(Modality, u64)>,
    pub output_by_modality: Vec<(Modality, u64)>,
    pub web_search_requests: Option<u64>,
    /// Every issue the procedure raised. A profile reports the subset that
    /// concerns its own points.
    pub issues: Issues,
    /// True when a missing cache part is what prevents the input total:
    /// `excludes_cache` with a cache part known, on an operation other than
    /// `embeddings`.
    pub missing_cache_part_prevents_total: bool,
}

fn count_fields(usage: &ReportedUsage) -> [(&'static str, Option<f64>); 9] {
    [
        ("input_tokens", usage.input_tokens),
        ("output_tokens", usage.output_tokens),
        ("cache_read_input_tokens", usage.cache_read_input_tokens),
        ("cache_write_input_tokens", usage.cache_write_input_tokens),
        (
            "cache_write_5m_input_tokens",
            usage.cache_write_5m_input_tokens,
        ),
        (
            "cache_write_1h_input_tokens",
            usage.cache_write_1h_input_tokens,
        ),
        ("reasoning_output_tokens", usage.reasoning_output_tokens),
        ("web_search_requests", usage.web_search_requests),
        (
            "web_search_requests_secondary",
            usage.web_search_requests_secondary,
        ),
    ]
}

fn modality_fields(usage: &ReportedUsage) -> [(&'static str, Option<&BTreeMap<String, f64>>); 2] {
    [
        (
            "input_tokens_by_modality",
            usage.input_tokens_by_modality.as_ref(),
        ),
        (
            "output_tokens_by_modality",
            usage.output_tokens_by_modality.as_ref(),
        ),
    ]
}

impl ReportedUsage {
    /// Step 2 of the order of checks: a count that is negative or
    /// fractional, in the field order the contract names.
    pub fn check_counts(&self) -> Result<(), MetricError> {
        for (name, value) in count_fields(self) {
            if value.is_some_and(|v| read_count(v) == CountReading::Malformed) {
                return reject(
                    ErrorCode::CountInvalid,
                    format!("{name} must be a non-negative integer"),
                );
            }
        }
        for (name, parts) in modality_fields(self) {
            for modality in Modality::REPORTED {
                let part = parts.and_then(|p| p.get(modality.as_str()));
                if part.is_some_and(|v| read_count(*v) == CountReading::Malformed) {
                    return reject(
                        ErrorCode::CountInvalid,
                        format!(
                            "{name}.{} must be a non-negative integer",
                            modality.as_str()
                        ),
                    );
                }
            }
        }
        Ok(())
    }

    /// Step 3 of the order of checks: the two bases, then a modality key
    /// outside `text`, `image`, `audio`.
    pub fn check_values(&self) -> Result<(), MetricError> {
        self.input_basis()?;
        self.output_basis()?;
        for (name, parts) in modality_fields(self) {
            let reported = |key: &String| Modality::REPORTED.iter().any(|m| m.as_str() == key);
            if let Some(key) = parts.and_then(|p| p.keys().find(|key| !reported(key))) {
                return reject(
                    ErrorCode::ValueNotAllowed,
                    format!("{name} has an unknown modality: {key}"),
                );
            }
        }
        Ok(())
    }

    fn input_basis(&self) -> Result<Option<InputBasis>, MetricError> {
        match self.input_basis.as_deref() {
            None => Ok(None),
            Some("includes_cache") => Ok(Some(InputBasis::IncludesCache)),
            Some("excludes_cache") => Ok(Some(InputBasis::ExcludesCache)),
            Some(_) => reject(
                ErrorCode::ValueNotAllowed,
                "input_basis must be includes_cache or excludes_cache",
            ),
        }
    }

    fn output_basis(&self) -> Result<Option<OutputBasis>, MetricError> {
        match self.output_basis.as_deref() {
            None => Ok(None),
            Some("includes_reasoning") => Ok(Some(OutputBasis::IncludesReasoning)),
            Some("excludes_reasoning") => Ok(Some(OutputBasis::ExcludesReasoning)),
            Some(_) => reject(
                ErrorCode::ValueNotAllowed,
                "output_basis must be includes_reasoning or excludes_reasoning",
            ),
        }
    }
}

/// Step 2 of the procedure: a count above 2^53-1 is treated as not reported.
fn bounded(value: Option<f64>, issues: &mut Issues) -> Option<u64> {
    match read_count(value?) {
        CountReading::Count(count) => Some(count),
        CountReading::AboveBound => {
            issues.insert(IssueCode::CountOutOfRange);
            None
        }
        // Rejected by `check_counts` before this point.
        CountReading::Malformed => None,
    }
}

/// A sum of counts, known only when it is at most 2^53-1.
fn bounded_sum(parts: &[u64], issues: &mut Issues) -> Option<u64> {
    let total = parts
        .iter()
        .try_fold(0u64, |sum, part| sum.checked_add(*part))
        .filter(|total| *total <= MAX_SAFE_INTEGER);
    if total.is_none() {
        issues.insert(IssueCode::CountOutOfRange);
    }
    total
}

fn total_of(parts: &[Option<u64>]) -> u128 {
    parts.iter().map(|part| u128::from(part.unwrap_or(0))).sum()
}

fn bounded_parts(
    parts: Option<&BTreeMap<String, f64>>,
    issues: &mut Issues,
) -> Vec<(Modality, u64)> {
    Modality::REPORTED
        .into_iter()
        .filter_map(|modality| {
            let part = parts.and_then(|p| p.get(modality.as_str())).copied();
            bounded(part, issues).map(|count| (modality, count))
        })
        .collect()
}

/// The Counter increments of one total: its reported parts and the
/// remainder as `unknown`, or the parts alone when the total is not known.
fn by_modality(
    total: Option<u64>,
    parts: Vec<(Modality, u64)>,
    total_missing: IssueCode,
    issues: &mut Issues,
) -> Vec<(Modality, u64)> {
    let Some(total) = total else {
        if !parts.is_empty() {
            issues.insert(total_missing);
        }
        return parts;
    };
    if parts.is_empty() {
        return vec![(Modality::Unknown, total)];
    }
    let reported = parts.iter().map(|(_, count)| u128::from(*count)).sum();
    let remainder = u128::from(total)
        .checked_sub(reported)
        .and_then(|rest| u64::try_from(rest).ok())
        .unwrap_or(0);
    let mut increments = parts;
    if remainder > 0 {
        increments.push((Modality::Unknown, remainder));
    }
    increments
}

/// Normalize the usage of one operation. `embeddings` is true for an
/// `embeddings` operation, which has no provider cache.
///
/// The malformed-observation checks run first, so the function is safe to
/// call on unchecked input; a projector that already ran them in its own
/// order of checks gets the same result.
pub fn normalize_usage(
    usage: &ReportedUsage,
    embeddings: bool,
) -> Result<NormalizedUsage, MetricError> {
    usage.check_counts()?;
    usage.check_values()?;
    let input_basis = usage.input_basis()?;
    let output_basis = usage.output_basis()?;
    let mut issues = Issues::new();

    let input = bounded(usage.input_tokens, &mut issues);
    let output = bounded(usage.output_tokens, &mut issues);
    let read = bounded(usage.cache_read_input_tokens, &mut issues);
    let write = bounded(usage.cache_write_input_tokens, &mut issues);
    let write_5m = bounded(usage.cache_write_5m_input_tokens, &mut issues);
    let write_1h = bounded(usage.cache_write_1h_input_tokens, &mut issues);
    let reasoning = bounded(usage.reasoning_output_tokens, &mut issues);
    let input_parts = bounded_parts(usage.input_tokens_by_modality.as_ref(), &mut issues);
    let output_parts = bounded_parts(usage.output_tokens_by_modality.as_ref(), &mut issues);
    let tool_primary = bounded(usage.web_search_requests, &mut issues);
    let tool_secondary = bounded(usage.web_search_requests_secondary, &mut issues);

    let cache_part_known = [read, write, write_5m, write_1h]
        .iter()
        .any(Option::is_some);
    // An `embeddings` operation has no provider cache: an absent cache part
    // is zero in sums, and is still no point.
    let in_sums = |part: Option<u64>| if embeddings { part.or(Some(0)) } else { part };

    if cache_part_known && input_basis.is_none() {
        issues.insert(IssueCode::InputBasisMissing);
    }
    if reasoning.is_some() && output_basis.is_none() {
        issues.insert(IssueCode::OutputBasisMissing);
    }
    if !embeddings {
        if read.is_none() {
            issues.insert(IssueCode::CacheReadMissing);
        }
        if write.is_none() {
            issues.insert(IssueCode::CacheWriteMissing);
        }
    }

    let inclusive_input = match (cache_part_known, input_basis) {
        (false, _) | (true, Some(InputBasis::IncludesCache)) => input,
        (true, None) => None,
        (true, Some(InputBasis::ExcludesCache)) => match (input, in_sums(read), in_sums(write)) {
            (Some(input), Some(read), Some(write)) => {
                bounded_sum(&[input, read, write], &mut issues)
            }
            _ => None,
        },
    };
    let inclusive_output = match (reasoning, output_basis) {
        (None, _) | (Some(_), Some(OutputBasis::IncludesReasoning)) => output,
        (Some(_), None) => None,
        (Some(reasoning), Some(OutputBasis::ExcludesReasoning)) => {
            output.and_then(|output| bounded_sum(&[output, reasoning], &mut issues))
        }
    };

    let lifetime_parts = total_of(&[write_5m, write_1h]);
    let cache_exceeds_input = cache_part_known
        && input_basis == Some(InputBasis::IncludesCache)
        && input.is_some_and(|input| {
            let writes = write.map_or(lifetime_parts, u128::from);
            total_of(&[read]) + writes > u128::from(input)
        });
    let lifetimes_exceed_writes = write.is_some_and(|write| lifetime_parts > u128::from(write));
    let exceeds = |parts: &[(Modality, u64)], total: Option<u64>| {
        total.is_some_and(|total| {
            parts.iter().map(|(_, c)| u128::from(*c)).sum::<u128>() > u128::from(total)
        })
    };
    let input_inconsistent =
        cache_exceeds_input || lifetimes_exceed_writes || exceeds(&input_parts, inclusive_input);
    let output_inconsistent = (output_basis == Some(OutputBasis::IncludesReasoning)
        && matches!((reasoning, output), (Some(r), Some(o)) if r > o))
        || exceeds(&output_parts, inclusive_output);

    let mut normalized = NormalizedUsage {
        missing_cache_part_prevents_total: cache_part_known
            && !embeddings
            && input_basis == Some(InputBasis::ExcludesCache),
        ..NormalizedUsage::default()
    };

    if input_inconsistent {
        issues.insert(IssueCode::UsageInconsistent);
    } else {
        normalized.input_total = inclusive_input;
        normalized.cache_read = read;
        normalized.cache_write_5m = write_5m;
        normalized.cache_write_1h = write_1h;
        normalized.cache_write_unspecified = write.and_then(|write| {
            let remainder = u128::from(write)
                .checked_sub(lifetime_parts)
                .and_then(|rest| u64::try_from(rest).ok())?;
            (remainder > 0 || (write_5m.is_none() && write_1h.is_none())).then_some(remainder)
        });
        if normalized
            .cache_write_unspecified
            .is_some_and(|rest| rest > 0)
        {
            issues.insert(IssueCode::CacheWriteLifetimeUnknown);
        }
        normalized.uncached_input = match (cache_part_known, input_basis) {
            (false, _) => input.filter(|_| embeddings),
            (true, None) => None,
            (true, Some(InputBasis::ExcludesCache)) => input,
            (true, Some(InputBasis::IncludesCache)) => {
                match (inclusive_input, in_sums(read), in_sums(write)) {
                    (Some(total), Some(read), Some(write)) => total
                        .checked_sub(read)
                        .and_then(|rest| rest.checked_sub(write)),
                    _ => None,
                }
            }
        };
        normalized.input_by_modality = by_modality(
            inclusive_input,
            input_parts,
            IssueCode::InputTotalMissing,
            &mut issues,
        );
    }

    if output_inconsistent {
        issues.insert(IssueCode::UsageInconsistent);
    } else {
        normalized.output_total = inclusive_output;
        normalized.reasoning = reasoning;
        normalized.output_by_modality = by_modality(
            inclusive_output,
            output_parts,
            IssueCode::OutputTotalMissing,
            &mut issues,
        );
    }

    normalized.web_search_requests = match (tool_primary, tool_secondary) {
        (Some(primary), Some(secondary)) if primary != secondary => {
            issues.insert(IssueCode::ServerToolCountConflict);
            None
        }
        (primary, secondary) => primary.or(secondary),
    };
    normalized.issues = issues;
    Ok(normalized)
}
