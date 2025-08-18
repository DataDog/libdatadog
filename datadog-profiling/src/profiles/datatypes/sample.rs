// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::SetId;
use crate::profiles::datatypes::attribute::AttributeSet;
use crate::profiles::datatypes::link::LinkSet;
use crate::profiles::datatypes::{AnyValue, KeyValue, Link, StackId, MAX_SAMPLE_TYPES};
use crate::profiles::string_writer::FallibleStringWriter;
use crate::profiles::ProfileError;
use arrayvec::ArrayVec;
use core::fmt::Write;
use std::borrow::Cow;

#[derive(Debug)]
pub struct Sample {
    pub stack_id: StackId,
    pub values: ArrayVec<i64, MAX_SAMPLE_TYPES>,
    pub attributes: Vec<SetId<KeyValue>>,
    pub link_id: Option<SetId<Link>>,
    pub timestamp_nanos: u64,
}

/// The SampleBuilder allows for building one or two values, and has helpers
/// for creating attributes to avoid allocations.
#[derive(Debug)]
pub struct SampleBuilder<'a> {
    attributes_set: &'a AttributeSet,
    link_set: &'a LinkSet,
    stack_id: Option<StackId>,
    values: ArrayVec<i64, MAX_SAMPLE_TYPES>,
    attributes: Vec<SetId<KeyValue>>,
    link_id: Option<SetId<Link>>,
    timestamp_nanos: Option<u64>,
}

impl<'a> SampleBuilder<'a> {
    pub fn new(attributes_set: &'a AttributeSet, link_set: &'a LinkSet) -> Self {
        Self {
            attributes_set,
            link_set,
            stack_id: None,
            values: ArrayVec::default(),
            attributes: Vec::new(),
            link_id: None,
            timestamp_nanos: None,
        }
    }

    pub fn stack_id(mut self, id: StackId) -> Self {
        self.stack_id = Some(id);
        self
    }

    /// Tries to add a value, failing if the maximum number of sample values
    /// per sample has been reached.
    pub fn value(mut self, value: i64) -> Result<Self, ProfileError> {
        match self.values.try_push(value) {
            Ok(_) => Ok(self),
            Err(_) => Err(ProfileError::InvalidInput),
        }
    }

    /// Attaches an attribute that already is in the [`KeyValue`] format. Use
    /// this in particular if you have a static key string and an integer
    /// value (nothing needs to allocate).
    #[inline(never)]
    pub fn attribute(mut self, key_value: KeyValue) -> Result<Self, ProfileError> {
        self.attributes.try_reserve(1)?;
        let id = self.attributes_set.try_insert(key_value)?;
        self.attributes.push(id);
        Ok(self)
    }

    /// Tries to build a [`KeyValue`] from the provided strings to use as the
    /// attribute. Handles memory failures gracefully instead of going
    /// out-of-memory.
    pub fn attribute_str(
        self,
        key: impl AsRef<str>,
        value: impl AsRef<str>,
    ) -> Result<Self, ProfileError> {
        let key = key.as_ref();
        if key.is_empty() {
            return Err(ProfileError::InvalidInput);
        }
        let mut key_writer = FallibleStringWriter::new();
        key_writer.try_reserve(key.len())?;
        write!(&mut key_writer, "{}", key).map_err(|_| ProfileError::OutOfMemory)?;
        let key_cow: Cow<'static, str> = Cow::Owned(String::from(key_writer));

        let value = value.as_ref();
        let mut w = FallibleStringWriter::new();
        w.try_reserve(value.len())
            .map_err(|_| ProfileError::OutOfMemory)?;
        write!(&mut w, "{}", value).map_err(|_| ProfileError::OutOfMemory)?;
        let value_owned = String::from(w);

        let key_value = KeyValue {
            key: key_cow,
            value: AnyValue::String(value_owned),
        };
        self.attribute(key_value)
    }

    /// Tries to build a [`KeyValue`] from the string and int to use as the
    /// attribute. Handles memory failures gracefully instead of going
    /// out-of-memory.
    pub fn attribute_int(self, key: impl AsRef<str>, value: i64) -> Result<Self, ProfileError> {
        let key = key.as_ref();
        if key.is_empty() {
            return Err(ProfileError::InvalidInput);
        }
        let mut w = FallibleStringWriter::new();
        w.try_reserve(key.len())?;
        write!(&mut w, "{}", key).map_err(|_| ProfileError::OutOfMemory)?;
        let key_cow: Cow<'static, str> = Cow::Owned(String::from(w));

        let key_value = KeyValue {
            key: key_cow,
            value: AnyValue::Integer(value),
        };
        self.attribute(key_value)
    }

    pub fn link(mut self, link: Link) -> Result<Self, ProfileError> {
        let id = self.link_set.try_insert(link)?;
        self.link_id = Some(id);
        Ok(self)
    }

    pub fn timestamp(mut self, timestamp_nanos: u64) -> Self {
        self.timestamp_nanos = Some(timestamp_nanos);
        self
    }

    pub fn build(self) -> Result<Sample, ProfileError> {
        let Some(stack_id) = self.stack_id else {
            return Err(ProfileError::InvalidInput);
        };

        Ok(Sample {
            stack_id,
            values: self.values,
            attributes: self.attributes,
            link_id: self.link_id,
            timestamp_nanos: self.timestamp_nanos.unwrap_or(0),
        })
    }
}
