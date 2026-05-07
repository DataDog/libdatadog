// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Backwards-compatibility shim for the legacy `libdd_common::http_common`
//! path. Prefer [`crate::http`] in new code.
//!
//! Every type and function previously defined here now lives in
//! [`crate::http`]; this module simply re-exports the same surface so existing
//! consumers continue to compile without source changes.
//!
//! Once all in-tree consumers have migrated to `libdd_common::http`, this
//! module is expected to gain a `#[deprecated]` attribute and eventually be
//! removed (see plan: M3-M7, Task 4 follow-up).

pub use crate::http::*;
