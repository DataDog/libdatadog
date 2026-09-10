// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::{borrow::Borrow, mem};

use libdd_trace_utils::span::SpanText;

use super::{obfuscate_sql, DbmsKind, SqlObfuscateConfig};

struct Entry<S> {
    dbms: DbmsKind,
    resource: S,
    // Retain the output only after the key repeats; unique-query batches otherwise need an extra
    // copy.
    obfuscated: Option<String>,
}

pub struct Cache<'a, S> {
    config: &'a SqlObfuscateConfig,
    last: Option<Entry<S>>,
}

impl<'a, S: SpanText> Cache<'a, S> {
    pub const fn new(config: &'a SqlObfuscateConfig) -> Self {
        Self { config, last: None }
    }

    pub fn obfuscate(&mut self, resource: &mut S, dbms: DbmsKind) -> String {
        let input = <S as Borrow<str>>::borrow(resource);
        let cached = self.last.as_ref().filter(|cached| {
            cached.dbms == dbms && <S as Borrow<str>>::borrow(&cached.resource) == input
        });
        if let Some(obfuscated) = cached.and_then(|cached| cached.obfuscated.as_ref()) {
            let obfuscated = obfuscated.clone();
            *resource = S::from_owned(obfuscated.clone());
            return obfuscated;
        }

        let cache_hit = cached.is_some();
        let obfuscated = obfuscate_sql(input, self.config, dbms);
        let mut cached_resource = S::from_owned(obfuscated.clone());
        mem::swap(resource, &mut cached_resource);
        self.last = Some(Entry {
            dbms,
            resource: cached_resource,
            obfuscated: cache_hit.then(|| obfuscated.clone()),
        });
        obfuscated
    }
}

#[cfg(test)]
mod tests {
    use alloc::borrow::Cow;

    use super::*;

    fn obfuscate(
        cache: &mut Cache<'_, Cow<'static, str>>,
        resource: &'static str,
        dbms: DbmsKind,
    ) -> String {
        let mut resource = Cow::Borrowed(resource);
        let obfuscated = cache.obfuscate(&mut resource, dbms);
        assert_eq!(resource.as_ref(), obfuscated);
        obfuscated
    }

    #[test]
    fn retains_output_after_a_repeated_key() {
        let config = SqlObfuscateConfig::default();
        let mut cache = Cache::new(&config);
        let resource = "SELECT * FROM users WHERE id = 42";

        assert_eq!(
            obfuscate(&mut cache, resource, DbmsKind::Generic),
            "SELECT * FROM users WHERE id = ?"
        );
        assert!(cache.last.as_ref().unwrap().obfuscated.is_none());

        assert_eq!(
            obfuscate(&mut cache, resource, DbmsKind::Generic),
            "SELECT * FROM users WHERE id = ?"
        );
        assert_eq!(
            cache.last.as_ref().unwrap().obfuscated.as_deref(),
            Some("SELECT * FROM users WHERE id = ?")
        );

        assert_eq!(
            obfuscate(&mut cache, resource, DbmsKind::Generic),
            "SELECT * FROM users WHERE id = ?"
        );
    }

    #[test]
    fn resets_retained_output_when_the_key_changes() {
        let config = SqlObfuscateConfig::default();
        let mut cache = Cache::new(&config);
        let resource = "SELECT a FROM foo WHERE value<@name";

        obfuscate(&mut cache, resource, DbmsKind::Generic);
        obfuscate(&mut cache, resource, DbmsKind::Generic);
        assert!(cache.last.as_ref().unwrap().obfuscated.is_some());

        assert_eq!(
            obfuscate(&mut cache, resource, DbmsKind::Postgresql),
            "SELECT a FROM foo WHERE value <@ name"
        );
        assert!(cache.last.as_ref().unwrap().obfuscated.is_none());

        assert_eq!(
            obfuscate(
                &mut cache,
                "SELECT * FROM users WHERE id = 42",
                DbmsKind::Postgresql,
            ),
            "SELECT * FROM users WHERE id = ?"
        );
        assert!(cache.last.as_ref().unwrap().obfuscated.is_none());
    }
}
