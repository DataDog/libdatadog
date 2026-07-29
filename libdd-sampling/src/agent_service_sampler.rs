// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use libdd_common::RwLockExt;

use crate::rate_sampler::RateSampler;

#[derive(Debug, serde::Deserialize)]
pub struct AgentRates<'a> {
    #[serde(borrow)]
    pub rate_by_service: Option<HashMap<&'a str, f64>>,
}

#[derive(Debug, Default, Clone)]
pub struct ServicesSampler {
    inner: Arc<RwLock<HashMap<String, RateSampler>>>,
}

impl ServicesSampler {
    pub fn get(&self, service: &str) -> Option<RateSampler> {
        self.inner.read_or_panic().get(service).cloned()
    }

    pub fn update_rates<I: IntoIterator<Item = (String, f64)>>(&self, rates: I) {
        let new_rates: HashMap<_, _> = rates
            .into_iter()
            .map(|(s, r)| (s, RateSampler::new(r)))
            .collect();
        *self.inner.write_or_panic() = new_rates;
    }

    // Test-only inspection helpers.

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.inner.read_or_panic().is_empty()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.inner.read_or_panic().len()
    }

    #[cfg(test)]
    pub(crate) fn contains_key(&self, service: &str) -> bool {
        self.inner.read_or_panic().contains_key(service)
    }
}
