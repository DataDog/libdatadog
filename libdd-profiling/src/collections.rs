// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Allocation and container building blocks: the [`Allocator`] API, a
//! hash table that lives in caller-provided bytes, and the insertion-ordered
//! map and set built on it.
//!
//! [`Allocator`]: alloc::Allocator

pub mod alloc;
pub mod hash_table;
pub mod index_map;
pub mod index_set;
pub mod vec;
