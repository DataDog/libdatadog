// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! Provides utilities to get config from the /info endpoint of an agent
#![deny(missing_docs)]

use std::sync::{Arc, LazyLock};

use arc_swap::ArcSwapOption;

pub mod schema;

mod fetcher;

static AGENT_INFO_CACHE: LazyLock<ArcSwapOption<schema::AgentInfo>> =
    LazyLock::new(|| ArcSwapOption::new(None));

/// Returns the most recent [`AgentInfo`] cached globally.
///
/// This function provides access to the latest [`AgentInfo`] that has been
/// fetched from the Datadog Agent's `/info` endpoint by the [`AgentInfoFetcher`].
/// The [`AgentInfo`] is stored in a global static cache that persists across thread
/// boundaries and process forks.
///
/// # Return Value
///
/// Returns `Some(Arc<AgentInfo>)` if an [`AgentInfo`] has been successfully
/// fetched at least once, or `None` if no [`AgentInfo`] is available yet.
pub fn get_agent_info() -> Option<Arc<schema::AgentInfo>> {
    AGENT_INFO_CACHE.load_full()
}

pub use fetcher::{
    check_response_for_new_state, fetch_info, fetch_info_with_state, AgentInfoFetcher,
    FetchInfoStatus,
};
