// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::{ParallelStringSet, StringId};
use crate::profiles::ProfileError;
use hashbrown::HashMap;

pub struct EndpointTracker {
    /// A set to deduplicate the endpoint strings, rather than hold the
    /// string twice, once in each map.
    strings: ParallelStringSet,
    trace_endpoints: parking_lot::Mutex<HashMap<i64, StringId>>,
    endpoint_counts: parking_lot::Mutex<HashMap<StringId, usize>>,
}

impl EndpointTracker {
    pub fn try_new() -> Result<Self, ProfileError> {
        Ok(Self {
            strings: ParallelStringSet::try_new()?,
            trace_endpoints: Default::default(),
            endpoint_counts: Default::default(),
        })
    }

    /// Adds a trace endpoint into the tracker. Does not add any counts.
    pub fn add_trace_endpoint(
        &self,
        local_root_span_id: i64,
        trace_endpoint: impl AsRef<str>,
    ) -> Result<StringId, ProfileError> {
        let str_id = self.strings.try_insert(trace_endpoint.as_ref())?;
        let mut guard = self.trace_endpoints.lock();
        guard.try_reserve(1)?;
        guard.insert(local_root_span_id, str_id);
        Ok(str_id)
    }

    /// Increment the endpoint count by `count`.
    pub fn add_endpoint_count(&self, str_id: StringId, count: usize) -> Result<(), ProfileError> {
        let mut guard = self.endpoint_counts.lock();
        guard.try_reserve(1)?;
        let entry = guard.entry_ref(&str_id);
        *entry.or_insert(0) += count;
        Ok(())
    }

    /// Adds a trace endpoint into the tracker, and increments its count.
    ///
    /// If the trace_endpoint already exists, and you know its ID, then use
    /// [`Self::add_endpoint_count`].
    pub fn add_trace_endpoint_with_count(
        &self,
        local_root_span_id: i64,
        trace_endpoint: impl AsRef<str>,
        count: usize,
    ) -> Result<StringId, ProfileError> {
        let str_id = self.add_trace_endpoint(local_root_span_id, trace_endpoint)?;
        self.add_endpoint_count(str_id, count)?;
        Ok(str_id)
    }
}
