// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::SetId;
use crate::profiles::collections::StringId;
use crate::profiles::datatypes::attribute::AttributeSet;
use crate::profiles::datatypes::link::LinkSet;
use crate::profiles::datatypes::{AnyValue, KeyValue, Link, StackId, MAX_SAMPLE_TYPES};
use crate::profiles::string_writer::FallibleStringWriter;
use crate::profiles::ProfileError;
use arrayvec::ArrayVec;
use core::fmt::Write;
use std::time::SystemTime;

#[derive(Debug)]
pub struct Sample {
    pub stack_id: StackId,
    pub values: ArrayVec<i64, MAX_SAMPLE_TYPES>,
    pub attributes: Vec<SetId<KeyValue>>,
    pub link_id: Option<SetId<Link>>,
    pub timestamp: Option<SystemTime>,
}

/// The SampleBuilder allows for building one or two values, and has helpers
/// for creating attributes to avoid allocations.
#[derive(Debug)]
pub struct SampleBuilder {
    attributes_set: AttributeSet,
    link_set: LinkSet,
    stack_id: Option<StackId>,
    values: ArrayVec<i64, MAX_SAMPLE_TYPES>,
    attributes: Vec<SetId<KeyValue>>,
    link_id: Option<SetId<Link>>,
    timestamp: Option<SystemTime>,
}

impl SampleBuilder {
    pub fn new(attributes_set: AttributeSet, link_set: LinkSet) -> Self {
        Self {
            attributes_set,
            link_set,
            stack_id: None,
            values: ArrayVec::default(),
            attributes: Vec::new(),
            link_id: None,
            timestamp: None,
        }
    }
    /// Sets the stack id in-place.
    pub fn set_stack_id(&mut self, id: StackId) {
        self.stack_id = Some(id);
    }

    /// Tries to add a value, failing if the maximum number of sample values
    /// per sample has been reached.
    pub fn push_value(&mut self, value: i64) -> Result<(), ProfileError> {
        match self.values.try_push(value) {
            Ok(_) => Ok(()),
            Err(_) => Err(ProfileError::InvalidInput),
        }
    }

    /// Attaches an attribute that already is in the [`KeyValue`] format. Use
    /// this in particular if you have a static key string and an integer
    /// value (nothing needs to allocate).
    #[inline(never)]
    pub fn push_attribute(&mut self, key_value: KeyValue) -> Result<(), ProfileError> {
        self.attributes.try_reserve(1)?;
        let id = self.attributes_set.try_insert(key_value)?;
        self.attributes.push(id);
        Ok(())
    }

    /// Tries to build a [`KeyValue`] from the provided strings to use as the
    /// attribute. Handles memory failures gracefully instead of going
    /// out-of-memory.
    pub fn push_attribute_str(
        &mut self,
        key_id: StringId,
        value: impl AsRef<str>,
    ) -> Result<(), ProfileError> {
        let value = value.as_ref();
        let mut w = FallibleStringWriter::new();
        w.try_reserve(value.len())
            .map_err(|_| ProfileError::OutOfMemory)?;
        write!(&mut w, "{}", value).map_err(|_| ProfileError::OutOfMemory)?;
        let value_owned = String::from(w);

        let key_value = KeyValue {
            key: key_id,
            value: AnyValue::String(value_owned),
        };
        self.push_attribute(key_value)
    }

    /// Tries to build a [`KeyValue`] from the string and int to use as the
    /// attribute. Handles memory failures gracefully instead of going
    /// out-of-memory.
    pub fn push_attribute_int(&mut self, key_id: StringId, value: i64) -> Result<(), ProfileError> {
        let key_value = KeyValue {
            key: key_id,
            value: AnyValue::Integer(value),
        };
        self.push_attribute(key_value)
    }

    pub fn set_link(&mut self, link: Link) -> Result<(), ProfileError> {
        let id = self.link_set.try_insert(link)?;
        self.link_id = Some(id);
        Ok(())
    }

    pub fn set_timestamp(&mut self, timestamp: SystemTime) {
        self.timestamp = Some(timestamp);
    }

    /// Build a Sample from current state and reset the builder for reuse.
    pub fn build(&mut self) -> Result<Sample, ProfileError> {
        let Some(stack_id) = self.stack_id.take() else {
            return Err(ProfileError::InvalidInput);
        };

        let values = core::mem::take(&mut self.values);
        let attributes = core::mem::take(&mut self.attributes);
        let link_id = self.link_id.take();
        let ts = self.timestamp.take();

        Ok(Sample {
            stack_id,
            values,
            attributes,
            link_id,
            timestamp: ts,
        })
    }
}
