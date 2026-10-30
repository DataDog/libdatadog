// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! This module implement the logic for sending telemetry/dogstatsd related to cardinality limits
// Because the cardinality limit RFC requires one point of telemetry with a set of collapsed fields
// and not one point for each collapsed field, this solution was found as an alternative to adding
// telemetry and dogstatsd client in the SpanConcentrator to keep it as "pure computation logic"

#[cfg(feature = "telemetry")]
use libdd_capabilities::{HttpClientCapability, MaybeSend, SleepCapability};
use libdd_common::tag::const_assert;

/// Bitset of collapsed stats key fields
pub struct CollapsedFieldSet(usize);
impl CollapsedFieldSet {
    pub const RESOURCE_NAME: usize = 1 << 0;
    pub const HTTP_ENDPOINT: usize = 1 << 1;
    pub const PEER_TAGS: usize = 1 << 2;
    pub const ADDITIONAL_TAGS: usize = 1 << 3;

    const FIELDS: [usize; 4] = [
        Self::RESOURCE_NAME,
        Self::HTTP_ENDPOINT,
        Self::PEER_TAGS,
        Self::ADDITIONAL_TAGS,
    ];

    /// Default value with none of the fields set (no collapsed fields)
    pub fn empty() -> CollapsedFieldSet {
        Self(0)
    }

    /// Enable the bit corresponding to `field`
    pub fn add(&mut self, field: usize) {
        debug_assert!(Self::FIELDS.contains(&field));
        self.0 |= field;
    }
}

const COLLAPSED_FIELD_METRIC_SIZE: usize = 1 << CollapsedFieldSet::FIELDS.len();
// Verify the array is of a reasonable size
const_assert!(COLLAPSED_FIELD_METRIC_SIZE <= 16);
/// Counter of combination of collapsed stats key fields
#[derive(Debug, Clone, Default, Copy)]
pub struct CollapsedFieldsMetrics {
    // Note: slot 0 is a counter for non_collapsed spans. It's not used for emitting telemetry
    collapsed: [usize; COLLAPSED_FIELD_METRIC_SIZE],
    /// Spans with additional metric tag values over the length limit. No field is collapsed for
    /// them, but they are reported on the same metric, tagged `oversized:additional_metric_tags`.
    oversized_additional_tags: usize,
}

impl CollapsedFieldsMetrics {
    /// Default value every combination of collapsed fields set to 0
    pub fn zero() -> Self {
        Self::default()
    }

    #[cfg(feature = "dogstatsd")]
    pub fn emit_dogstatsd(&self, dogstatsd: &libdd_dogstatsd_client::DogStatsDClient) {
        // skip the first slot that is used to count span which have no collapsed fields
        for (mask, &count) in self.collapsed.iter().enumerate().skip(1) {
            if count > 0 {
                let tags = Self::fields_mask_to_list(mask);
                dogstatsd.send(vec![libdd_dogstatsd_client::DogStatsDAction::Count(
                    "datadog.tracer.stats.collapsed_spans",
                    count as i64,
                    tags.iter(),
                )]);
            }
        }
        if self.oversized_additional_tags > 0 {
            dogstatsd.send(vec![libdd_dogstatsd_client::DogStatsDAction::Count(
                "datadog.tracer.stats.collapsed_spans",
                self.oversized_additional_tags as i64,
                [Self::oversized_additional_tags_tag()].iter(),
            )]);
        }
    }

    #[cfg(feature = "telemetry")]
    pub fn emit_telemetry<
        Cap: HttpClientCapability + SleepCapability + MaybeSend + Sync + 'static,
    >(
        &self,
        handle: &libdd_telemetry::worker::TelemetryWorkerHandle<Cap>,
        context_key: &libdd_telemetry::metrics::ContextKey,
    ) {
        // skip the first slot that is used to count span which have no collapsed fields
        for (mask, &count) in self.collapsed.iter().enumerate().skip(1) {
            if count > 0 {
                let tags = Self::fields_mask_to_list(mask);
                let _ = handle.add_point(count as f64, context_key, tags);
            }
        }
        if self.oversized_additional_tags > 0 {
            let _ = handle.add_point(
                self.oversized_additional_tags as f64,
                context_key,
                vec![Self::oversized_additional_tags_tag()],
            );
        }
    }

    #[cfg(any(feature = "telemetry", feature = "dogstatsd"))]
    fn oversized_additional_tags_tag() -> libdd_common::tag::Tag {
        libdd_common::tag!("oversized", "additional_metric_tags")
    }

    /// Given a bitmask of collapsed fields, returns the list of tags to attach to
    /// telemetry/dogstatsd
    #[cfg(any(feature = "telemetry", feature = "dogstatsd"))]
    fn fields_mask_to_list(mask: usize) -> Vec<libdd_common::tag::Tag> {
        let mut tags = Vec::new();
        for field_pow in 0..CollapsedFieldSet::FIELDS.len() {
            let field_value = 1 << field_pow;
            debug_assert!(
                CollapsedFieldSet::FIELDS.contains(&field_value),
                "{field_value} is an invalid value for a CollapsedFieldSet"
            );
            let has_field = (mask & field_value) != 0;
            if !has_field {
                continue;
            }
            let field_tag = match field_value {
                CollapsedFieldSet::RESOURCE_NAME => {
                    libdd_common::tag!("collapsed", "resource")
                }
                CollapsedFieldSet::HTTP_ENDPOINT => {
                    libdd_common::tag!("collapsed", "http_endpoint")
                }
                CollapsedFieldSet::PEER_TAGS => {
                    libdd_common::tag!("collapsed", "peer_tags")
                }
                CollapsedFieldSet::ADDITIONAL_TAGS => {
                    libdd_common::tag!("collapsed", "additional_metric_tags")
                }
                // Should be unreachable, but don't fail in prod if provided with an invalid field
                // set
                _ => continue,
            };
            tags.push(field_tag);
        }
        debug_assert!(!tags.is_empty());
        tags
    }

    /// Increment the telemetry counter corresponding to this collapsed field combination
    pub fn increment(&mut self, field_set: CollapsedFieldSet) {
        self.add(field_set, 1);
    }

    /// Add `count` to the telemetry counter corresponding to this collapsed field combination
    pub fn add(&mut self, field_set: CollapsedFieldSet, count: usize) {
        self.collapsed[field_set.0] += count;
    }

    /// Add `count` spans with additional metric tag values over the length limit
    pub fn add_oversized_additional_tags(&mut self, count: usize) {
        self.oversized_additional_tags += count;
    }
}

impl std::ops::AddAssign for CollapsedFieldsMetrics {
    fn add_assign(&mut self, rhs: Self) {
        for i in 0..self.collapsed.len() {
            self.collapsed[i] += rhs.collapsed[i];
        }
        self.oversized_additional_tags += rhs.oversized_additional_tags;
    }
}
