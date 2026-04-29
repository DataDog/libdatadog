// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

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
        self.inner.read().unwrap().get(service).cloned()
    }

    pub fn update_rates<I: IntoIterator<Item = (String, f64)>>(&self, rates: I) {
        let new_rates: HashMap<_, _> = rates
            .into_iter()
            .map(|(s, r)| (s, RateSampler::new(r)))
            .collect();
        *self.inner.write().unwrap() = new_rates;
    }

    // used for testing purposes

    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.inner.read().unwrap().is_empty()
    }

    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    #[allow(dead_code)]
    pub(crate) fn contains_key(&self, service: &str) -> bool {
        self.inner.read().unwrap().contains_key(service)
    }
}
