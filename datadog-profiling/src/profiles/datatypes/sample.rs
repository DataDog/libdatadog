// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::SetId;
use crate::profiles::datatypes::attribute::AttributeSet;
use crate::profiles::datatypes::link::LinkSet;
use crate::profiles::datatypes::{AnyValue, KeyValue, Link, StackId, MAX_SAMPLE_TYPES};
use crate::profiles::string_writer::FallibleStringWriter;
use crate::profiles::ProfileError;
use arrayvec::ArrayVec;
use std::borrow::Cow;

#[derive(Debug)]
pub struct Sample {
    pub stack_id: StackId,
    pub values: ArrayVec<i64, MAX_SAMPLE_TYPES>,
    pub attributes: Vec<SetId<KeyValue>>,
    pub link_id: Option<SetId<Link>>,
    pub timestamp_nanos: u64,
}

#[derive(Debug)]
pub struct SampleBuilder {
    attributes_set: AttributeSet,
    link_set: LinkSet,
    stack_id: Option<StackId>,
    values: ArrayVec<i64, MAX_SAMPLE_TYPES>,
    attributes: Vec<SetId<KeyValue>>,
    link_id: Option<SetId<Link>>,
    timestamp_nanos: Option<u64>,
}

impl SampleBuilder {
    pub fn try_new(attributes_set: AttributeSet, link_set: LinkSet) -> Self {
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

    pub fn value(mut self, value: i64) -> Result<Self, ProfileError> {
        match self.values.try_push(value) {
            Ok(_) => Ok(self),
            Err(_) => Err(ProfileError::InvalidInput),
        }
    }

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
        // Prefer borrowed static keys if possible
        let key_cow: Cow<'static, str> =
            if let Some(static_key) = if key.is_empty() { Some("") } else { None } {
                Cow::Borrowed(static_key)
            } else {
                let s = FallibleStringWriter::try_format_with_size_hint(&key, key.len())
                    .map_err(|_| ProfileError::OutOfMemory)?;
                Cow::Owned(s)
            };

        let value = value.as_ref();
        let value_owned = FallibleStringWriter::try_format_with_size_hint(&value, value.len())
            .map_err(|_| ProfileError::OutOfMemory)?;

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
        let key_cow: Cow<'static, str> =
            if let Some(static_key) = if key.is_empty() { Some("") } else { None } {
                Cow::Borrowed(static_key)
            } else {
                let s = FallibleStringWriter::try_format_with_size_hint(&key, key.len())
                    .map_err(|_| ProfileError::OutOfMemory)?;
                Cow::Owned(s)
            };

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

    pub fn finish(self) -> Result<Sample, ProfileError> {
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
