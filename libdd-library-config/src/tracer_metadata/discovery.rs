// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Linux metadata discovery using rustix, available without `std`.
//!
//! Filesystem access, namespace inode lookups, and hostname discovery are handled internally.
//! Environment variables come from [`environment`]'s owned startup snapshot. Results are not
//! cached.

#[cfg(target_os = "linux")]
pub use libdd_common::entity_id::{discover as discover_origin, read_container_id, read_hostname};
pub use libdd_common::{
    entity_id::{EntityInfo, parse_container_id},
    environment,
};
