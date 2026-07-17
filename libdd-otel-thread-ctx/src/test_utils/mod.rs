// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Test utilities used by this crate's own tests and `gen_tls_shim_hash` dev tool, and by
//! `libdd-otel-thread-ctx-ffi`'s `elf_properties` integration test.

pub mod artifacts;
pub mod tls_shim_window;
