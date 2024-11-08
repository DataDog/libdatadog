// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! Provides utilities to get config from the /info endpoint of an agent
#![deny(missing_docs)]

use std::sync::Arc;

use arc_swap::ArcSwapOption;

pub mod schema;

mod fetcher;

/// Stores an AgentInfo in an ArcSwap to be updated by an AgentInfoFetcher
pub type AgentInfoArc = Arc<ArcSwapOption<schema::AgentInfo>>;

pub use fetcher::{fetch_info, fetch_info_with_state, AgentInfoFetcher, FetchInfoStatus};
