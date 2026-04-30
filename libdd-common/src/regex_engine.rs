// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Workspace-wide regex engine re-exports.
//!
//! By default this module re-exports from the full [`regex`] crate.
//! Enable the **`regex-lite`** feature to switch to [`regex_lite`] instead,
//! which trades advanced features (Unicode classes, look-around, etc.) for
//! smaller binary size and faster compile times.

#[cfg(feature = "regex-lite")]
pub use regex_lite::{escape, Captures, Regex, RegexBuilder, Replacer};

#[cfg(not(feature = "regex-lite"))]
pub use regex::{escape, Captures, Regex, RegexBuilder, Replacer};
