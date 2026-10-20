// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A v0.4-compatible view of a span's attributes.

use crate::{
    AttributeRef, Attributes, AttributesIter, COMPONENT_KEY, ENV_KEY, PROMOTED_SPAN_TAGS,
    SPAN_KIND_KEY, Span, VERSION_KEY,
};

/// Wraps a span so that its attributes include the promoted span tags (see
/// [`PROMOTED_SPAN_TAGS`]), the way v0.4 `meta` would hold them: e.g.
/// `attribute_str("env")` reads [`Span::env`].
///
/// This should only be used where tags are looked up by a name that isn't known statically, to
/// stay backwards compatible with v0.4: e.g. user-configured peer tags, trace filters or sampling
/// rules. Code that knows which tag it wants should call the dedicated getter instead.
///
/// Promoted tags are strings: the numeric getters never see them. The promoted chunk tags (e.g.
/// `_dd.origin`) are hidden by default in v0.4 and get no such compatibility.
///
/// Wrap a [`ChunkSpanView`](crate::ChunkSpanView) to also see the chunk attributes.
#[repr(transparent)]
pub struct CompatSpan<S>(pub S);

impl<S> CompatSpan<S> {
    /// Wraps a borrowed span.
    pub const fn from_ref(span: &S) -> &Self {
        // SAFETY: `CompatSpan` is a `repr(transparent)` wrapper around `S`, so `&S` and
        // `&CompatSpan<S>` have the same layout, and the lifetime is carried over.
        unsafe { &*core::ptr::from_ref(span).cast::<Self>() }
    }
}

fn is_promoted(key: &str) -> bool {
    PROMOTED_SPAN_TAGS.contains(&key)
}

impl<S: Span> CompatSpan<S> {
    /// The value of the promoted tag `key`, `None` if it is unset or `key` isn't a promoted tag.
    fn promoted(&self, key: &str) -> Option<&str> {
        match key {
            SPAN_KIND_KEY => self.0.span_kind(),
            ENV_KEY => self.0.env(),
            VERSION_KEY => self.0.version(),
            COMPONENT_KEY => self.0.component(),
            _ => None,
        }
    }
}

impl<S: Span> Attributes for CompatSpan<S> {
    type Text = S::Text;
    type Bytes = S::Bytes;
    type Values = S::Values;

    fn attribute_str(&self, key: &str) -> Option<&str> {
        if is_promoted(key) {
            self.promoted(key)
        } else {
            self.0.attribute_str(key)
        }
    }

    fn attribute_f64(&self, key: &str) -> Option<f64> {
        if is_promoted(key) {
            None
        } else {
            self.0.attribute_f64(key)
        }
    }

    fn attribute_i64(&self, key: &str) -> Option<i64> {
        if is_promoted(key) {
            None
        } else {
            self.0.attribute_i64(key)
        }
    }
}

/// Yields the span's attributes, then its promoted tags.
impl<S: Span + AttributesIter> AttributesIter for CompatSpan<S> {
    fn attributes(&self) -> impl Iterator<Item = (&str, AttributeRef<'_, Self::Values>)> {
        let promoted = PROMOTED_SPAN_TAGS.into_iter().filter_map(|key| {
            let value = self.promoted(key)?;
            Some((key, AttributeRef::String(value)))
        });
        // Spans never hold promoted tags in their attributes, but a `ChunkSpanView` may expose
        // chunk attributes under those names: the getters already take them into account.
        self.0
            .attributes()
            .filter(|(key, _)| !is_promoted(key))
            .chain(promoted)
    }

    fn has_attribute(&self, key: &str) -> bool {
        if is_promoted(key) {
            self.promoted(key).is_some()
        } else {
            self.0.has_attribute(key)
        }
    }
}

impl<S: Span> Span for CompatSpan<S> {
    type SpanEvent = S::SpanEvent;
    type SpanLink = S::SpanLink;

    fn service(&self) -> &str {
        self.0.service()
    }

    fn resource(&self) -> &str {
        self.0.resource()
    }

    fn name(&self) -> &str {
        self.0.name()
    }

    fn typ(&self) -> &str {
        self.0.typ()
    }

    fn span_id(&self) -> u64 {
        self.0.span_id()
    }

    fn parent_id(&self) -> u64 {
        self.0.parent_id()
    }

    fn start(&self) -> i64 {
        self.0.start()
    }

    fn duration(&self) -> i64 {
        self.0.duration()
    }

    fn is_error(&self) -> bool {
        self.0.is_error()
    }

    fn span_kind(&self) -> Option<&str> {
        self.0.span_kind()
    }

    fn env(&self) -> Option<&str> {
        self.0.env()
    }

    fn version(&self) -> Option<&str> {
        self.0.version()
    }

    fn component(&self) -> Option<&str> {
        self.0.component()
    }

    fn span_events(&self) -> impl Iterator<Item = &Self::SpanEvent> {
        self.0.span_events()
    }

    fn span_links(&self) -> impl Iterator<Item = &Self::SpanLink> {
        self.0.span_links()
    }

    // The markers are read from the wrapped span, which may override them.
    fn is_trace_root(&self) -> bool {
        self.0.is_trace_root()
    }

    fn has_top_level(&self) -> bool {
        self.0.has_top_level()
    }

    fn is_measured(&self) -> bool {
        self.0.is_measured()
    }

    fn is_partial_snapshot(&self) -> bool {
        self.0.is_partial_snapshot()
    }
}
