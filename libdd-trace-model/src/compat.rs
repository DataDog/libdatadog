// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! A v0.4-compatible view of a span's attributes, for reads and writes.

use crate::{
    AttributeRef, AttributeValue, Attributes, AttributesIter, AttributesMut, COMPONENT_KEY,
    ENV_KEY, PROMOTED_SPAN_TAGS, SPAN_KIND_KEY, Span, SpanMut, TraceText, VERSION_KEY, Value,
    is_promoted,
};

/// Wraps a span so that its attributes include the promoted span tags (see
/// [`PROMOTED_SPAN_TAGS`]), the way v0.4 `meta` would hold them: e.g.
/// `attribute_str("env")` reads [`Span::env`].
///
/// This should only be used where tags are looked up or written by a name that isn't known
/// statically, to stay backwards compatible with v0.4: e.g. user-configured peer tags, trace
/// filters, sampling rules or tag replacement rules. Code that knows which tag it wants should call
/// the dedicated getter or setter instead. Get a mutable view with
/// [`SpanMut::compat_mut`](crate::SpanMut::compat_mut).
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

    /// Wraps a mutably borrowed span.
    pub const fn from_mut(span: &mut S) -> &mut Self {
        // SAFETY: as in `from_ref`; the exclusive borrow is carried over.
        unsafe { &mut *core::ptr::from_mut(span).cast::<Self>() }
    }
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
        // v0.4 spans also hold the promoted tags in their attributes: yield them once, from the
        // getters.
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

/// Writes to a promoted tag go to its setter, dropping values the field can't hold (e.g. a
/// non-string `env`), and removing one clears its field. Both also drop any attribute of the same
/// name, which the getters hide (e.g. a tag a decoder didn't promote), so the field is left as the
/// only value.
///
/// The promoted fields can't be handed out as an [`AttributeValue`]:
/// [`attribute_mut`](AttributesMut::attribute_mut) and
/// [`retain_attributes`](AttributesMut::retain_attributes) only reach the other attributes, so
/// unlike [`AttributesIter::attributes`] they never see the promoted tags. To edit a promoted tag
/// by name, read it through [`Span::compat`], copy it (the shared borrow can't outlive the
/// mutable one), and write it back with [`set_attribute`](AttributesMut::set_attribute).
impl<S: SpanMut> AttributesMut for CompatSpan<S> {
    fn retain_attributes(&mut self, mut f: impl FnMut(&str, &mut Value<Self>) -> bool) {
        // Attributes named like a promoted tag are hidden by the getters: keep them untouched.
        self.0
            .retain_attributes(|key, value| is_promoted(key) || f(key, value));
    }

    fn attribute_mut(&mut self, key: &str) -> Option<&mut Value<Self>> {
        if is_promoted(key) {
            None
        } else {
            self.0.attribute_mut(key)
        }
    }

    fn set_attribute(&mut self, key: impl Into<Self::Text>, value: Value<Self>) {
        let key = key.into();
        if !is_promoted(key.as_str()) {
            return self.0.set_attribute(key, value);
        }
        let AttributeValue::String(text) = value else {
            // A value the promoted field can't hold.
            return;
        };
        match key.as_str() {
            SPAN_KIND_KEY => self.0.set_span_kind(text.as_str()),
            ENV_KEY => self.0.set_env(text),
            VERSION_KEY => self.0.set_version(text),
            COMPONENT_KEY => self.0.set_component(text),
            _ => {}
        }
        self.0.remove_attribute(key.as_str());
    }

    fn remove_attribute(&mut self, key: &str) -> Option<Value<Self>> {
        if !is_promoted(key) {
            return self.0.remove_attribute(key);
        }
        // The getters only lend the value out, so it is copied before the field is cleared.
        let old = self
            .promoted(key)
            .map(|value| AttributeValue::String(S::Text::from(value.to_owned())));
        match key {
            SPAN_KIND_KEY => self.0.set_span_kind(""),
            ENV_KEY => self.0.set_env(S::Text::from_static("")),
            VERSION_KEY => self.0.set_version(S::Text::from_static("")),
            COMPONENT_KEY => self.0.set_component(S::Text::from_static("")),
            _ => {}
        }
        self.0.remove_attribute(key);
        old
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

impl<S: SpanMut> SpanMut for CompatSpan<S> {
    fn set_service(&mut self, value: impl Into<Self::Text>) {
        self.0.set_service(value);
    }

    fn set_resource(&mut self, value: impl Into<Self::Text>) {
        self.0.set_resource(value);
    }

    fn set_name(&mut self, value: impl Into<Self::Text>) {
        self.0.set_name(value);
    }

    fn set_type(&mut self, value: impl Into<Self::Text>) {
        self.0.set_type(value);
    }

    fn set_parent_id(&mut self, value: u64) {
        self.0.set_parent_id(value);
    }

    fn set_start(&mut self, value: i64) {
        self.0.set_start(value);
    }

    fn set_duration(&mut self, value: i64) {
        self.0.set_duration(value);
    }

    fn set_error(&mut self, value: bool) {
        self.0.set_error(value);
    }

    fn set_span_kind(&mut self, value: &str) {
        self.0.set_span_kind(value);
    }

    fn set_env(&mut self, value: impl Into<Self::Text>) {
        self.0.set_env(value);
    }

    fn set_version(&mut self, value: impl Into<Self::Text>) {
        self.0.set_version(value);
    }

    fn set_component(&mut self, value: impl Into<Self::Text>) {
        self.0.set_component(value);
    }

    fn span_events_mut(&mut self) -> impl Iterator<Item = &mut Self::SpanEvent> {
        self.0.span_events_mut()
    }

    fn span_links_mut(&mut self) -> impl Iterator<Item = &mut Self::SpanLink> {
        self.0.span_links_mut()
    }
}
