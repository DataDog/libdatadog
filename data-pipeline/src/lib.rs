// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![deny(missing_docs)]

//! TraceExporter provides a minimum viable product (MVP) to send traces to agents. The aim of the project at this
//! state is to provide a basic API in order to test its viability and integration in different languages.

/// Span Concentrator provides a method to "concentrate" span stats together.
pub mod span_concentrator;
/// Trace Exporter allows accepts trace payloads and exports them to the datadog agent.
pub mod trace_exporter;

