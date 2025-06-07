// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

//! TraceExporter provides a minimum viable product (MVP) to send traces to agents. The aim of the
//! project at this state is to provide a basic API in order to test its viability and integration
//! in different languages.

pub mod agent_info;
mod health_metrics;
mod pausable_worker;
#[allow(missing_docs)]
pub mod span_concentrator;
#[allow(missing_docs)]
pub mod stats_exporter;
pub mod telemetry;
#[allow(missing_docs)]
pub mod trace_exporter;
