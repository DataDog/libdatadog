// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OpenTelemetry emitter for converting datadog-profiling internal types to OpenTelemetry protobuf
//! types
//!
//! This module provides `From` trait implementations for converting internal types to their
//! OpenTelemetry protobuf equivalents.

pub mod function;
pub mod label;
pub mod location;
pub mod mapping;
pub mod profile;
pub mod stack_trace;
