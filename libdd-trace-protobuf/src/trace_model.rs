// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Read-only [`libdd_trace_model`] trait implementations for the protobuf spans.
//!
//! Like v0.4 spans, protobuf spans keep strings and numbers in the separate `meta` and `metrics`
//! maps, which the typed reads look up directly, and have no dedicated fields for the promoted
//! tags (`span.kind`, `env`, ...): those are regular `meta` tags.

use crate::pb;
use crate::pb::attribute_any_value::AttributeAnyValueType;
use libdd_trace_model::{Attributes, StringData};

impl Attributes for pb::Span {
    type Text = String;
    type Bytes = Vec<u8>;
    type Values = StringData;

    fn attribute_str(&self, key: &str) -> Option<&str> {
        self.meta.get(key).map(String::as_str)
    }

    fn attribute_f64(&self, key: &str) -> Option<f64> {
        self.metrics.get(key).copied()
    }
}

impl libdd_trace_model::Span for pb::Span {
    type SpanEvent = pb::SpanEvent;
    type SpanLink = pb::SpanLink;

    fn service(&self) -> &str {
        &self.service
    }

    fn resource(&self) -> &str {
        &self.resource
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn typ(&self) -> &str {
        &self.r#type
    }

    fn span_id(&self) -> u64 {
        self.span_id
    }

    fn parent_id(&self) -> u64 {
        self.parent_id
    }

    fn start(&self) -> i64 {
        self.start
    }

    fn duration(&self) -> i64 {
        self.duration
    }

    fn is_error(&self) -> bool {
        self.error != 0
    }

    fn span_kind(&self) -> Option<&str> {
        self.attribute_str(libdd_trace_model::SPAN_KIND_KEY)
    }

    fn env(&self) -> Option<&str> {
        self.attribute_str(libdd_trace_model::ENV_KEY)
    }

    fn version(&self) -> Option<&str> {
        self.attribute_str(libdd_trace_model::VERSION_KEY)
    }

    fn component(&self) -> Option<&str> {
        self.attribute_str(libdd_trace_model::COMPONENT_KEY)
    }

    fn span_events(&self) -> impl Iterator<Item = &Self::SpanEvent> {
        self.span_events.iter()
    }

    fn span_links(&self) -> impl Iterator<Item = &Self::SpanLink> {
        self.span_links.iter()
    }
}

/// Span links only carry string attributes.
impl Attributes for pb::SpanLink {
    type Text = String;
    type Bytes = Vec<u8>;
    type Values = StringData;

    fn attribute_str(&self, key: &str) -> Option<&str> {
        self.attributes.get(key).map(String::as_str)
    }

    fn attribute_f64(&self, _key: &str) -> Option<f64> {
        None
    }
}

impl libdd_trace_model::SpanLink for pb::SpanLink {
    fn trace_id(&self) -> u128 {
        (u128::from(self.trace_id_high) << 64) | u128::from(self.trace_id)
    }

    fn span_id(&self) -> u64 {
        self.span_id
    }

    fn tracestate(&self) -> &str {
        &self.tracestate
    }

    fn flags(&self) -> u32 {
        self.flags
    }
}

/// Typed reads only see single values; array attributes and unknown value types read as missing.
impl Attributes for pb::SpanEvent {
    type Text = String;
    type Bytes = Vec<u8>;
    type Values = StringData;

    fn attribute_str(&self, key: &str) -> Option<&str> {
        let value = self.attributes.get(key)?;
        match AttributeAnyValueType::try_from(value.r#type).ok()? {
            AttributeAnyValueType::StringValue => Some(&value.string_value),
            _ => None,
        }
    }

    fn attribute_f64(&self, key: &str) -> Option<f64> {
        let value = self.attributes.get(key)?;
        match AttributeAnyValueType::try_from(value.r#type).ok()? {
            AttributeAnyValueType::DoubleValue => Some(value.double_value),
            // Lossy above 2^53, which is acceptable for the metric-like values read as floats.
            #[allow(clippy::cast_precision_loss)]
            AttributeAnyValueType::IntValue => Some(value.int_value as f64),
            _ => None,
        }
    }

    fn attribute_i64(&self, key: &str) -> Option<i64> {
        let value = self.attributes.get(key)?;
        match AttributeAnyValueType::try_from(value.r#type).ok()? {
            AttributeAnyValueType::IntValue => Some(value.int_value),
            AttributeAnyValueType::DoubleValue => {
                libdd_trace_model::f64_to_exact_i64(value.double_value)
            }
            _ => None,
        }
    }
}

impl libdd_trace_model::SpanEvent for pb::SpanEvent {
    fn name(&self) -> &str {
        &self.name
    }

    fn time_unix_nano(&self) -> u64 {
        self.time_unix_nano
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_trace_model::{Span as _, SpanLink as _};
    use std::collections::HashMap;

    fn event_value(r#type: AttributeAnyValueType) -> pb::AttributeAnyValue {
        pb::AttributeAnyValue {
            r#type: r#type as i32,
            ..Default::default()
        }
    }

    #[test]
    fn span_reads_meta_and_metrics() {
        let span = pb::Span {
            parent_id: 0,
            error: 1,
            meta: HashMap::from([
                ("shared".to_owned(), "str".to_owned()),
                ("span.kind".to_owned(), "client".to_owned()),
                ("env".to_owned(), "prod".to_owned()),
            ]),
            metrics: HashMap::from([("shared".to_owned(), 2.0), ("_dd.measured".to_owned(), 1.0)]),
            ..Default::default()
        };
        // A key in both maps stays reachable through both typed reads.
        assert_eq!(span.attribute_str("shared"), Some("str"));
        assert_eq!(span.attribute_f64("shared"), Some(2.0));
        // Promoted tags are plain meta tags.
        assert_eq!(span.span_kind(), Some("client"));
        assert_eq!(span.env(), Some("prod"));
        assert_eq!(span.version(), None);
        assert!(span.is_measured());
        assert!(span.is_error());
        assert!(span.is_trace_root());
        assert!(!span.has_top_level());
    }

    #[test]
    fn links_and_events_read_attributes() {
        let link = pb::SpanLink {
            trace_id: 2,
            trace_id_high: 1,
            attributes: HashMap::from([("k".to_owned(), "v".to_owned())]),
            ..Default::default()
        };
        assert_eq!(link.trace_id(), (1u128 << 64) | 2);
        assert_eq!(link.attribute_str("k"), Some("v"));

        let event = pb::SpanEvent {
            attributes: HashMap::from([
                (
                    "s".to_owned(),
                    pb::AttributeAnyValue {
                        string_value: "v".to_owned(),
                        ..event_value(AttributeAnyValueType::StringValue)
                    },
                ),
                (
                    "int".to_owned(),
                    pb::AttributeAnyValue {
                        int_value: i64::MAX,
                        ..event_value(AttributeAnyValueType::IntValue)
                    },
                ),
                (
                    "frac".to_owned(),
                    pb::AttributeAnyValue {
                        double_value: 4.5,
                        ..event_value(AttributeAnyValueType::DoubleValue)
                    },
                ),
                (
                    "array".to_owned(),
                    event_value(AttributeAnyValueType::ArrayValue),
                ),
            ]),
            ..Default::default()
        };
        assert_eq!(event.attribute_str("s"), Some("v"));
        // A string read doesn't see the default `string_value` of a non-string value.
        assert_eq!(event.attribute_str("int"), None);
        assert_eq!(event.attribute_i64("int"), Some(i64::MAX));
        assert_eq!(event.attribute_i64("frac"), None);
        assert_eq!(event.attribute_f64("frac"), Some(4.5));
        assert_eq!(event.attribute_f64("array"), None);

        let span = pb::Span {
            span_links: vec![link],
            span_events: vec![event],
            ..Default::default()
        };
        assert_eq!(span.span_links().count(), 1);
        assert_eq!(span.span_events().count(), 1);
    }
}
