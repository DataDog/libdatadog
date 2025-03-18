// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

pub mod aggregator;
pub mod config;
pub mod env_verifier;
pub mod http_utils;
pub mod mini_agent;
pub mod stats_flusher;
pub mod stats_processor;
pub mod trace_flusher;
pub mod trace_processor;
