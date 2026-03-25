use rustc_hash::FxHashMap;

#[derive(Default)]
pub struct Trace<T> {
    pub meta: FxHashMap<T, T>,
    pub metrics: FxHashMap<T, f64>,
    pub origin: Option<T>,
    pub sampling_rule_decision: Option<f64>,
    pub sampling_limit_decision: Option<f64>,
    pub sampling_agent_decision: Option<f64>,
    pub span_count: usize,
}

/// A map optimized for the common case of 1 active trace.
///
/// In typical Node.js applications, there is usually only one active trace
/// at a time (single-threaded, one request in flight). This structure stores
/// the first trace inline — no heap allocation, no hashing — and falls back
/// to a `Vec` for the rare case of multiple concurrent traces. A `Vec` with
/// linear scan beats `FxHashMap` for small N (<~20) due to better cache
/// locality and zero hashing overhead for u128 keys.
pub struct SmallTraceMap<T> {
    /// The first (and usually only) trace, stored inline.
    inline_key: u128,
    inline_val: Option<Trace<T>>,
    /// Overflow storage for additional traces. Only allocated when there are
    /// 2+ concurrent traces.
    overflow: Vec<(u128, Trace<T>)>,
}

impl<T> Default for SmallTraceMap<T> {
    fn default() -> Self {
        SmallTraceMap {
            inline_key: 0,
            inline_val: None,
            overflow: Vec::new(),
        }
    }
}

impl<T> SmallTraceMap<T> {
    /// Returns true if the map contains no traces.
    pub fn is_empty(&self) -> bool {
        self.inline_val.is_none()
    }

    /// Get an immutable reference to a trace by ID.
    #[inline]
    pub fn get(&self, key: &u128) -> Option<&Trace<T>> {
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

    /// Get a mutable reference to a trace by ID.
    #[inline]
    pub fn get_mut(&mut self, key: &u128) -> Option<&mut Trace<T>> {
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

    /// Get a mutable reference to a trace, inserting a default if not present.
    /// This is the equivalent of `HashMap::entry(key).or_default()`.
    #[inline]
    pub fn get_or_insert_default(&mut self, key: u128) -> &mut Trace<T>
    where
        T: Default,
    {
        // Hot path: inline slot matches this key (most common after first
        // Create — all subsequent Creates for the same trace hit this).
        if self.inline_key == key {
            if self.inline_val.is_some() {
                return self.inline_val.as_mut().unwrap();
            }
            // inline_key matched but slot was empty — populate it
            self.inline_val = Some(Trace::default());
            return self.inline_val.as_mut().unwrap();
        }

        // Inline slot has a different trace. Check overflow.
        if self.inline_val.is_none() {
            // Inline slot is empty — use it for this key
            self.inline_key = key;
            self.inline_val = Some(Trace::default());
            return self.inline_val.as_mut().unwrap();
        }

        // Slow path: linear scan overflow
        let pos = self.overflow.iter().position(|(k, _)| *k == key);
        match pos {
            Some(i) => &mut self.overflow[i].1,
            None => {
                self.overflow.push((key, Trace::default()));
                &mut self.overflow.last_mut().unwrap().1
            }
        }
    }

    /// Remove a trace by ID and return it.
    pub fn remove(&mut self, key: &u128) -> Option<Trace<T>> {
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
