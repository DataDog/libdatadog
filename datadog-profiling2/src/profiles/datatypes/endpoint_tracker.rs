// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::profiles::collections::{ParallelStringSet, StringId2};
use crate::profiles::{FallibleStringWriter, ProfileError};
use hashbrown::HashMap;

pub struct EndpointTracker {
    /// A set to deduplicate the endpoint strings, rather than hold the
    /// string twice, once in each map.
    strings: ParallelStringSet,
    trace_endpoints: parking_lot::Mutex<HashMap<i64, StringId2>>,
    endpoint_counts: parking_lot::Mutex<HashMap<StringId2, usize>>,
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
    ) -> Result<StringId2, ProfileError> {
        let str_id = self.strings.try_insert(trace_endpoint.as_ref())?;
        let mut guard = self.trace_endpoints.lock();
        guard.try_reserve(1)?;
        guard.insert(local_root_span_id, str_id);
        Ok(str_id)
    }

    /// Increment the endpoint count by `count`.
    pub fn add_endpoint_count(&self, str_id: StringId2, count: usize) -> Result<(), ProfileError> {
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
    ) -> Result<StringId2, ProfileError> {
        let str_id = self.add_trace_endpoint(local_root_span_id, trace_endpoint)?;
        self.add_endpoint_count(str_id, count)?;
        Ok(str_id)
    }

    /// Returns the endpoint string for a given local root span id, if present.
    /// The returned string is borrowed from internal storage; no allocation.
    pub fn get_trace_endpoint_str(&self, local_root_span_id: i64) -> Option<&str> {
        let str_id = {
            let guard = self.trace_endpoints.lock();
            guard.get(&local_root_span_id).copied()
        }?;
        // SAFETY: string ids refer to entries in `strings`
        Some(unsafe { self.strings.get(str_id) })
    }

    // todo: can we avoid copying?
    pub fn get_endpoint_stats(&self) -> Result<HashMap<String, i64>, ProfileError> {
        let mut stats = HashMap::<String, i64>::new();
        let strings = &self.strings;
        let guard = self.endpoint_counts.lock();
        stats.try_reserve(guard.len())?;
        for (id, count) in guard.iter() {
            let mut writer = FallibleStringWriter::new();
            writer.try_push_str(unsafe { strings.get(*id) })?;
            stats.insert(String::from(writer), (*count) as i64);
        }
        Ok(stats)
    }

    /// Exposes the string table--use responsibly, it's only meant to hold
    /// strings related to endpoints. Don't put unrelated strings in here.
    /// Mostly exposed for FFI's sake.
    pub fn strings(&self) -> &ParallelStringSet {
        &self.strings
    }
}
