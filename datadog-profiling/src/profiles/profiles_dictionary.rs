// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::ProfileId;
use crate::profiles::collections::{SliceTable, StringTable, Table};
use crate::profiles::{Function, Link, Location, Mapping};
use datadog_alloc::AllocError;
use std::alloc::LayoutError;
use std::collections;

/*
Data that is long-lived:
 1. Mappings, and their strings.
 2. Functions, and their strings.

Data that is short-lived:
 1. Attributes/labels and their strings.
 2. Locations. File + line is a lot of potential unique locations
 3. Links?

 */

pub struct ProfilesDictionary {
    mapping_table: Table<Mapping>,
    location_table: Table<Location>,
    function_table: Table<Function>,
    link_table: Table<Link>,
    string_table: StringTable,
    stack_table: SliceTable<ProfileId>,
}

#[derive(thiserror::Error, Debug)]
pub enum DictionaryError {
    #[error("invalid input")]
    InvalidInput,
    #[error("container full or out of memory")]
    OutOfMemory,
}

impl From<LayoutError> for DictionaryError {
    fn from(_: LayoutError) -> DictionaryError {
        DictionaryError::InvalidInput
    }
}

impl From<AllocError> for DictionaryError {
    fn from(_: AllocError) -> DictionaryError {
        DictionaryError::OutOfMemory
    }
}

impl From<collections::TryReserveError> for DictionaryError {
    fn from(_: collections::TryReserveError) -> DictionaryError {
        DictionaryError::OutOfMemory
    }
}

impl From<hashbrown::TryReserveError> for DictionaryError {
    fn from(_: hashbrown::TryReserveError) -> DictionaryError {
        DictionaryError::OutOfMemory
    }
}
