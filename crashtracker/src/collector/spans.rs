// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use portable_atomic::{AtomicU128, AtomicUsize};
use rand::Rng;
use std::io::Write;
use std::sync::atomic::Ordering::SeqCst;

static ACTIVE_SPANS: AtomicU128Set<2048> = AtomicU128Set::new();
static ACTIVE_TRACES: AtomicU128Set<2048> = AtomicU128Set::new();

pub fn clear_spans() -> anyhow::Result<()> {
    ACTIVE_SPANS.clear()
}

#[allow(dead_code)]
pub fn emit_spans(w: &mut impl Write) -> anyhow::Result<()> {
    use crate::shared::constants::*;
    writeln!(w, "{DD_CRASHTRACK_BEGIN_SPAN_IDS}")?;
    ACTIVE_SPANS.emit(w)?;
    writeln!(w, "{DD_CRASHTRACK_END_SPAN_IDS}")?;
    w.flush()?;
    Ok(())
}

pub fn insert_span(value: u128) -> anyhow::Result<usize> {
    ACTIVE_SPANS.insert(value)
}

pub fn remove_span(value: u128, idx: usize) -> anyhow::Result<()> {
    ACTIVE_SPANS.remove(value, idx)
}

pub fn clear_traces() -> anyhow::Result<()> {
    ACTIVE_TRACES.clear()
}

#[allow(dead_code)]
pub fn emit_traces(w: &mut impl Write) -> anyhow::Result<()> {
    use crate::shared::constants::*;
    writeln!(w, "{DD_CRASHTRACK_BEGIN_TRACE_IDS}")?;
    ACTIVE_TRACES.emit(w)?;
    writeln!(w, "{DD_CRASHTRACK_END_TRACE_IDS}")?;
    w.flush()?;
    Ok(())
}

pub fn insert_trace(value: u128) -> anyhow::Result<usize> {
    ACTIVE_TRACES.insert(value)
}

pub fn remove_trace(value: u128, idx: usize) -> anyhow::Result<()> {
    ACTIVE_TRACES.remove(value, idx)
}

struct AtomicU128Set<const LEN: usize> {
    used: AtomicUsize,
    set: [AtomicU128; LEN],
}

#[allow(dead_code)]
impl<const LEN: usize> AtomicU128Set<LEN> {
    /// Atomicity: This is NOT ATOMIC.  If other code modifies the set while this is happening,
    /// badness will occur.
    pub fn clear(&self) -> anyhow::Result<()> {
        if self.is_empty() {
            for v in self.set.iter() {
                let old = v.swap(0, SeqCst);
                if old != 0 {
                    self.used.sub(1, SeqCst)
                }
            }
        }
        Ok(())
    }

    pub fn emit(&self, w: &mut impl Write) -> anyhow::Result<()> {
        write!(w, "[")?;

        if self.used.load(SeqCst) > 0 {
            let mut first = true;
            for it in self.set.iter() {
                let v = it.load(SeqCst);
                if v != 0 {
                    if !first {
                        write!(w, ", ")?;
                    }
                    first = false;
                    write!(w, "{{\"id\": \"{v}\"}}")?;
                }
            }
        }
        writeln!(w, "]")?;

        Ok(())
    }

    pub const fn new() -> Self {
        // In this case, we actually WANT multiple copies of the interior mutable struct
        #[allow(clippy::declare_interior_mutable_const)]
        const ATOMIC_ZERO: AtomicU128 = AtomicU128::new(0);
        Self {
            used: AtomicUsize::new(0),
            set: [ATOMIC_ZERO; LEN],
        }
    }

    /// Add
    pub fn insert(&self, value: u128) -> anyhow::Result<usize> {
        let used = self.used.fetch_add(1, SeqCst);
        if used >= self.set.len() / 2 {
            // We only fill to half full to get good amortized behaviour
            self.used.fetch_sub(1, SeqCst);
            anyhow::bail!("Crashtracker: No space to store span {value}");
        }

        // Start at a random position.
        // Since the array is only at most half full, and since we start scanning at random
        // indicies, every slot should independently have <.5 probability of being occupied.
        // Long scans become exponentially unlikely, giving amortized constant time insertion.
        let shift: usize = rand::thread_rng().gen_range(0..self.set.len());
        for i in 0..self.set.len() {
            let idx = (i + shift) % self.set.len();
            if self.set[idx]
                .compare_exchange(0, value, SeqCst, SeqCst)
                .is_ok()
            {
                return Ok(idx);
            }
        }
        anyhow::bail!("This should be unreachable: we ensure that there was at least one empty slot before entering the loop")
    }

    pub fn is_empty(&self) -> bool {
        self.len() != 0
    }

    pub fn len(&self) -> usize {
        self.used.load(SeqCst)
    }

    pub fn remove(&self, value: u128, idx: usize) -> anyhow::Result<()> {
        anyhow::ensure!(idx < self.set.len(), "Idx {idx} out of range");
        match self.set[idx].compare_exchange(value, 0, SeqCst, SeqCst) {
            Ok(_) => {
                self.used.fetch_sub(1, SeqCst);
                Ok(())
            }
            Err(old) => {
                anyhow::bail!("Invalid index/span_id pair: Expected {value} at {idx}, got {old}")
            }
        }
    }

    pub fn values(&self) -> anyhow::Result<Vec<u128>> {
        let mut rval = Vec::with_capacity(self.used.load(SeqCst));
        if self.used.load(SeqCst) > 0 {
            for it in self.set.iter() {
                let v = it.load(SeqCst);
                if v != 0 {
                    rval.push(v);
                }
            }
        }
        rval.sort();
        Ok(rval)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() -> anyhow::Result<()> {
        let s: AtomicU128Set<16> = AtomicU128Set::new();
        assert_eq!(s.len(), 0);
        assert_eq!(&s.values()?, &[]);
        Ok(())
    }

    #[test]
    fn test_ops() -> anyhow::Result<()> {
        let mut expected = std::collections::BTreeMap::<u128, usize>::new();
        let s: AtomicU128Set<8> = AtomicU128Set::new();
        compare(&s, &expected);
        insert_and_compare(&s, &mut expected, 42);
        insert_and_compare(&s, &mut expected, 21);
        insert_and_compare(&s, &mut expected, 19);
        insert_and_compare(&s, &mut expected, 3);
        insert(&s, &mut expected, 8).expect_err("Should stop when half full");

        s.remove(42, 200)
            .expect_err("Shouldn't let us go outside the range");
        // Try removing a value at the wrong idx, nothing happens.
        let idx = expected.get(&42).unwrap();
        s.remove(43, (*idx + 1) % s.len()).unwrap_err();
        compare(&s, &expected);

        remove_and_compare(&s, &mut expected, 42);
        insert_and_compare(&s, &mut expected, 12);
        remove_and_compare(&s, &mut expected, 19);

        s.clear()?;
        expected.clear();
        compare(&s, &expected);
        insert_and_compare(&s, &mut expected, 12);

        Ok(())
    }

    #[test]
    fn test_emit() {
        let s: AtomicU128Set<8> = AtomicU128Set::new();
        s.insert(42).unwrap();
        s.insert(21).unwrap();
        let mut buf = Vec::new();
        s.emit(&mut buf).unwrap();
        let actual = String::from_utf8(buf).unwrap();
        assert!(
            actual == "[{\"id\": \"42\"}, {\"id\": \"21\"}]\n"
                || actual == "[{\"id\": \"21\"}, {\"id\": \"42\"}]\n"
        );
    }

    fn remove_and_compare(
        s: &AtomicU128Set<8>,
        expected: &mut std::collections::BTreeMap<u128, usize>,
        v: u128,
    ) {
        remove(s, expected, v).unwrap();
        compare(s, expected);
    }

    fn remove(
        s: &AtomicU128Set<8>,
        expected: &mut std::collections::BTreeMap<u128, usize>,
        v: u128,
    ) -> anyhow::Result<()> {
        let idx = expected.get(&v).unwrap();
        s.remove(v, *idx).unwrap();
        expected.remove(&v);
        Ok(())
    }

    fn compare(s: &AtomicU128Set<8>, expected: &std::collections::BTreeMap<u128, usize>) {
        let actual = s.values().unwrap();
        let golden: Vec<u128> = expected.keys().cloned().collect();
        assert_eq!(actual, golden);
        assert_eq!(expected.len(), s.len());
    }

    fn insert(
        s: &AtomicU128Set<8>,
        expected: &mut std::collections::BTreeMap<u128, usize>,
        v: u128,
    ) -> anyhow::Result<()> {
        expected.insert(v, s.insert(v)?);
        Ok(())
    }

    fn insert_and_compare(
        s: &AtomicU128Set<8>,
        expected: &mut std::collections::BTreeMap<u128, usize>,
        v: u128,
    ) {
        insert(s, expected, v).unwrap();
        compare(s, expected);
    }
}
