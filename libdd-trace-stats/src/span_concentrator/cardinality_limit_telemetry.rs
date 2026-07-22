// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Each collapsed field is assigned a bit; a span's mask is the OR of the bits for the
//! fields collapsed on it, so masks double as an index into
//! [`CollapsedFieldsMetrics`]'s counter array (one counter per distinct field combination).

#[cfg(feature = "telemetry")]
use libdd_capabilities::{HttpClientCapability, MaybeSend, SleepCapability};
use libdd_common::tag::const_assert;

pub(super) mod collapsed_field {
    pub const RESOURCE_NAME: usize = 1 << 1;
    pub const HTTP_ENDPOINT: usize = 1 << 2;
    pub const PEER_TAGS: usize = 1 << 3;
    pub const ADDITIONAL_TAGS: usize = 1 << 4;
    pub const COUNT: u8 = 5;
}

pub(super) const COLLAPSED_FIELD_METRIC_SIZE: usize = 1 << collapsed_field::COUNT;

#[derive(Debug, Clone, Default, Copy)]
// Note: slot 0 is a counter for non_collapsed spans. It's not used for emitting telemetry
pub struct CollapsedFieldsMetrics(pub(super) [usize; COLLAPSED_FIELD_METRIC_SIZE]);

const_assert!(COLLAPSED_FIELD_METRIC_SIZE <= 32);

impl CollapsedFieldsMetrics {
    pub fn zero() -> Self {
        Self::default()
    }

    #[cfg(feature = "dogstatsd")]
    pub fn emit_dogstatsd(&self, dogstatsd: &libdd_dogstatsd_client::DogStatsDClient) {
        // skip the first slot that is used to count span which have no collapsed fields.
        for (mask, &count) in self.0.iter().enumerate().skip(1) {
            if count > 0 {
                let tags = Self::fields_mask_to_list(mask);
                dogstatsd.send(vec![libdd_dogstatsd_client::DogStatsDAction::Count(
                    "datadog.tracer.stats.collapsed_spans",
                    count as i64,
                    tags.iter(),
                )]);
            }
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
        // skip the first slot that is used to count span which have no collapsed fields.
        for (mask, &count) in self.0.iter().enumerate().skip(1) {
            if count > 0 {
                let tags = Self::fields_mask_to_list(mask);
                let _ = handle.add_point(count as f64, context_key, tags);
            }
        }
    }

    /// Given a bitmask of collapsed fields, returns the list of tags to attach to
    /// telemetry/dogstatsd
    #[cfg(any(feature = "telemetry", feature = "dogstatsd"))]
    fn fields_mask_to_list(mask: usize) -> Vec<libdd_common::tag::Tag> {
        let mut tags = Vec::new();
        for field_pow in 1..collapsed_field::COUNT {
            let field_value = 1 << field_pow;
            debug_assert!([
                collapsed_field::RESOURCE_NAME,
                collapsed_field::HTTP_ENDPOINT,
                collapsed_field::PEER_TAGS,
                collapsed_field::ADDITIONAL_TAGS
            ]
            .contains(&field_value));
            let has_field = (mask & field_value) != 0;
            if !has_field {
                continue;
            }
            let field_tag = match field_value {
                collapsed_field::RESOURCE_NAME => {
                    libdd_common::tag!("collapsed_spans", "resource")
                }
                collapsed_field::HTTP_ENDPOINT => {
                    libdd_common::tag!("collapsed_spans", "http_endpoint")
                }
                collapsed_field::PEER_TAGS => {
                    libdd_common::tag!("collapsed_spans", "peer_tags")
                }
                collapsed_field::ADDITIONAL_TAGS => {
                    libdd_common::tag!("collapsed_spans", "additional_metric_tags")
                }
                // Unreachable: asserted just above that field is one of the 4 possible values
                _ => continue,
            };
            tags.push(field_tag);
        }
        assert!(!tags.is_empty());
        tags
    }
}

impl std::ops::AddAssign for CollapsedFieldsMetrics {
    fn add_assign(&mut self, rhs: Self) {
        for i in 0..self.0.len() {
            self.0[i] += rhs.0[i];
        }
    }
}
