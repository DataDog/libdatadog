// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! Provides utilities to get config from the /info endpoint of an agent
#![deny(missing_docs)]

use std::{ops::Deref, sync::Arc};

use arc_swap::ArcSwapOption;

pub mod schema;

mod fetcher;

/// Stores an AgentInfo in an ArcSwap to be updated by an AgentInfoFetcher
#[derive(Debug, Default, Clone)]
pub struct AgentInfoCell(Arc<ArcSwapOption<schema::AgentInfo>>);

impl AgentInfoCell {
    /// load the Arc contained into the cell
    pub fn load(&self) -> impl Deref<Target = Option<Arc<schema::AgentInfo>>> {
        self.0.load()
    }

    /// store a new value into the cell
    pub fn store(&self, v: Option<schema::AgentInfo>) {
        self.0.store(v.map(Arc::new));
    }
}

pub use fetcher::{
    fetch_info, fetch_info_with_state, AgentInfoFetcher, FetchInfoStatus, ResponseObserver,
};
