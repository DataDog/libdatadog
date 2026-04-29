// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::sync::{Arc, RwLock};

use super::sampling_rule::SamplingRule;

/// Thread-safe container for sampling rules
#[derive(Debug, Default, Clone)]
pub(crate) struct RulesSampler {
    inner: Arc<RwLock<Vec<SamplingRule>>>,
}

impl RulesSampler {
    pub fn new(rules: Vec<SamplingRule>) -> Self {
        Self {
            inner: Arc::new(RwLock::new(rules)),
        }
    }

    /// Updates the rules with a new set
    pub fn update_rules(&self, new_rules: Vec<SamplingRule>) {
        *self.inner.write().unwrap() = new_rules;
    }

    /// Finds the first matching rule for a span
    pub fn find_matching_rule<F>(&self, matcher: F) -> Option<SamplingRule>
    where
        F: Fn(&SamplingRule) -> bool,
    {
        self.inner
            .read()
            .unwrap()
            .iter()
            .find(|rule| matcher(rule))
            .cloned()
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
}
