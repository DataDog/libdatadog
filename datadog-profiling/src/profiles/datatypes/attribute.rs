// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Attributes are key value pairs where the value can be one of a few
//! different types. Strings in the attributes are not interned in otel, and
//! currently the module does not deduplicate them either.

use crate::profiles::collections::{ParallelSet, StringId};
use std::ffi::c_void;

/// Represents possible values of key value types. Note that otel supports
/// more types than this.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum AnyValue {
    String(String),
    Integer(i64),
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct KeyValue {
    pub key: StringId,
    pub value: AnyValue,
}

pub type AttributeSet = ParallelSet<KeyValue, 4>;

// Avoid NonNull<()> in FFI; see PR:
// https://github.com/mozilla/cbindgen/pull/1098
pub type AttributeId = std::ptr::NonNull<c_void>;
