// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::vec_map::VecMap;

/// Per-segment state for a trace chunk.
///
/// A segment is an independent visit by a service to a distributed trace. Multiple
/// segments may share the same `trace_id` (e.g. A → B → A creates two segments for
/// service A). Each segment has its own isolated metadata so that trace-level operations
/// on one segment do not affect another.
#[derive(Default)]
pub struct Segment<T> {
    pub meta: VecMap<T, T>,
    pub metrics: VecMap<T, f64>,
    pub origin: Option<T>,
    pub sampling_rule_decision: Option<f64>,
    pub sampling_limit_decision: Option<f64>,
    pub sampling_agent_decision: Option<f64>,
    pub span_count: usize,
}

/// A map optimized for the common case of 1 active segment.
///
/// In typical Node.js applications, there is usually only one active segment
/// at a time (single-threaded, one request in flight). This structure stores
/// the first segment inline — no heap allocation, no hashing — and falls back
/// to a `Vec` for the rare case of multiple concurrent segments. A `Vec` with
/// linear scan beats `FxHashMap` for small N (<~20) due to better cache
/// locality and zero hashing overhead for u64 keys.
pub struct SmallSegmentMap<T> {
    /// The first (and usually only) segment, stored inline.
    inline_key: u64,
    inline_val: Option<Segment<T>>,
    /// Overflow storage for additional segments. Only allocated when there are
    /// 2+ concurrent segments.
    overflow: Vec<(u64, Segment<T>)>,
}

impl<T> Default for SmallSegmentMap<T> {
    fn default() -> Self {
        SmallSegmentMap {
            inline_key: 0,
            inline_val: None,
            overflow: Vec::new(),
        }
    }
}

impl<T> SmallSegmentMap<T> {
    /// Returns true if the map contains no segments.
    pub fn is_empty(&self) -> bool {
        self.inline_val.is_none()
    }

    /// Get an immutable reference to a segment by ID.
    #[inline]
    pub fn get(&self, key: &u64) -> Option<&Segment<T>> {
        if let Some(ref val) = self.inline_val {
            if self.inline_key == *key {
                return Some(val);
            }
        }
        self.overflow
            .iter()
            .find(|(k, _)| *k == *key)
            .map(|(_, v)| v)
    }

    /// Get a mutable reference to a segment by ID.
    #[inline]
    pub fn get_mut(&mut self, key: &u64) -> Option<&mut Segment<T>> {
        if let Some(ref mut val) = self.inline_val {
            if self.inline_key == *key {
                return Some(val);
            }
        }
        self.overflow
            .iter_mut()
            .find(|(k, _)| *k == *key)
            .map(|(_, v)| v)
    }

    /// Get a mutable reference to a segment, inserting a default if not present.
    /// This is the equivalent of `HashMap::entry(key).or_default()`.
    #[inline]
    pub fn get_or_insert_default(&mut self, key: u64) -> &mut Segment<T>
    where
        T: Default,
    {
        // Hot path: inline slot matches this key or is empty.
        if self.inline_key == key || self.inline_val.is_none() {
            self.inline_key = key;
            return self.inline_val.get_or_insert_with(Segment::default);
        }

        // Slow path: linear scan overflow
        let pos = self.overflow.iter().position(|(k, _)| *k == key);
        match pos {
            Some(i) => &mut self.overflow[i].1,
            None => {
                self.overflow.push((key, Segment::default()));
                let last = self.overflow.len() - 1;
                &mut self.overflow[last].1
            }
        }
    }

    /// Remove a segment by ID and return it.
    pub fn remove(&mut self, key: &u64) -> Option<Segment<T>> {
        if self.inline_val.is_some() && self.inline_key == *key {
            let val = self.inline_val.take();
            // If there's overflow, promote the last entry to inline
            if let Some((k, v)) = self.overflow.pop() {
                self.inline_key = k;
                self.inline_val = Some(v);
            }
            return val;
        }
        let pos = self.overflow.iter().position(|(k, _)| *k == *key)?;
        Some(self.overflow.swap_remove(pos).1)
    }
}
