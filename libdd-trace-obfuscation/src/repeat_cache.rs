// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

struct Entry<K, V> {
    key: K,
    value: Option<V>,
}

pub struct RepeatCache<K, V> {
    last: Option<Entry<K, V>>,
}

impl<K: PartialEq, V: Clone> RepeatCache<K, V> {
    pub const fn new() -> Self {
        Self { last: None }
    }

    pub fn resolve_with(&mut self, key: K, compute: impl FnOnce(&K) -> V) -> V {
        let cached = self.last.as_ref().filter(|entry| entry.key == key);
        if let Some(value) = cached.and_then(|entry| entry.value.as_ref()) {
            return value.clone();
        }

        let repeated = cached.is_some();
        let value = compute(&key);
        self.last = Some(Entry {
            key,
            // Retain the output only after the key repeats; unique-key workloads otherwise need
            // an extra copy.
            value: repeated.then(|| value.clone()),
        });
        value
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::*;

    fn resolve(
        cache: &mut RepeatCache<&'static str, String>,
        computations: &Cell<usize>,
        key: &'static str,
    ) -> String {
        cache.resolve_with(key, |key| {
            computations.set(computations.get() + 1);
            format!("{key}-{}", computations.get())
        })
    }

    #[test]
    fn retains_the_second_result_for_adjacent_matches() {
        let computations = Cell::new(0);
        let mut cache = RepeatCache::new();

        assert_eq!(resolve(&mut cache, &computations, "a"), "a-1");
        assert_eq!(resolve(&mut cache, &computations, "a"), "a-2");
        assert_eq!(resolve(&mut cache, &computations, "a"), "a-2");
        assert_eq!(computations.get(), 2);
    }

    #[test]
    fn discards_the_retained_result_when_the_key_changes() {
        let computations = Cell::new(0);
        let mut cache = RepeatCache::new();

        assert_eq!(resolve(&mut cache, &computations, "a"), "a-1");
        assert_eq!(resolve(&mut cache, &computations, "a"), "a-2");
        assert_eq!(resolve(&mut cache, &computations, "b"), "b-3");
        assert_eq!(resolve(&mut cache, &computations, "a"), "a-4");
        assert_eq!(computations.get(), 4);
    }
}
