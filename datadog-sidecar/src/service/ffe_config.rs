// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::shm_remote_config::cdn::CdnPublisher;
use libdd_common::MutexExt;
use libdd_ffe::cdn::{CdnConfig, CdnFetcher};
use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, Weak};
use tokio_util::sync::CancellationToken;

/// Connections own subscriptions. The cache only holds weak references, so the
/// last disconnect cancels polling and releases the configuration/credentials.
#[derive(Clone, Default)]
pub(crate) struct CdnConfigs(Arc<Mutex<HashMap<CdnConfig, Weak<CdnSubscription>>>>);

pub(crate) struct CdnSubscription {
    pub(crate) config: CdnConfig,
    pub(crate) path: String,
    cancellation: CancellationToken,
    cache: Weak<Mutex<HashMap<CdnConfig, Weak<CdnSubscription>>>>,
}

impl Drop for CdnSubscription {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(cache) = self.cache.upgrade() {
            let mut cache = cache.lock_or_panic();
            if cache
                .get(&self.config)
                .is_some_and(|entry| entry.strong_count() == 0)
            {
                cache.remove(&self.config);
            }
        }
    }
}

impl CdnConfigs {
    pub(crate) fn subscribe(&self, config: CdnConfig) -> io::Result<Arc<CdnSubscription>> {
        config.validate().map_err(io::Error::other)?;
        let mut subscriptions = self.0.lock_or_panic();
        // Expired keys own credentials too; remove them before admitting work.
        subscriptions.retain(|_, value| value.strong_count() != 0);
        if let Some(subscription) = subscriptions.get(&config).and_then(Weak::upgrade) {
            return Ok(subscription);
        }
        let (publisher, path) = CdnPublisher::new()?;
        let mut fetcher =
            CdnFetcher::new(config.clone(), publisher.clone()).map_err(io::Error::other)?;
        let cancellation = CancellationToken::new();
        let subscription = Arc::new(CdnSubscription {
            config: config.clone(),
            path,
            cancellation: cancellation.clone(),
            cache: Arc::downgrade(&self.0),
        });
        subscriptions.insert(config, Arc::downgrade(&subscription));
        tokio::spawn(async move {
            fetcher.run(cancellation).await;
            publisher.stopped();
        });
        Ok(subscription)
    }

    pub(crate) fn shutdown(&self) {
        let subscriptions: Vec<_> = self.0.lock_or_panic().drain().collect();
        for (_, subscription) in subscriptions {
            if let Some(subscription) = subscription.upgrade() {
                subscription.cancellation.cancel();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_ffe::cdn::CdnEndpoint;
    use std::time::Duration;

    fn config(key: &str, environment: &str) -> CdnConfig {
        CdnConfig {
            endpoint: CdnEndpoint::managed("datadoghq.com", environment, key).unwrap(),
            language: "php".into(),
            version: "test".into(),
            poll_interval: Duration::from_secs(30),
            request_timeout: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore = "requires native shared memory")]
    async fn cdn_subscribers_share_only_matching_credentials_and_environment() {
        // No yield before cancellation: no managed request is made by this test.
        let configs = CdnConfigs::default();
        let first = configs
            .subscribe(config("first-key", "production"))
            .unwrap();
        let second = configs
            .subscribe(config("first-key", "production"))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        let other_key = configs
            .subscribe(config("second-key", "production"))
            .unwrap();
        let other_env = configs.subscribe(config("first-key", "staging")).unwrap();
        assert_ne!(first.path, other_key.path);
        assert_ne!(first.path, other_env.path);
        let cancelled = first.cancellation.clone();
        let path = first.path.clone();
        drop(first);
        assert!(!cancelled.is_cancelled());
        drop(second);
        assert!(cancelled.is_cancelled());
        assert!(
            !configs
                .0
                .lock_or_panic()
                .contains_key(&config("first-key", "production"))
        );
        let replacement = configs
            .subscribe(config("first-key", "production"))
            .unwrap();
        assert_ne!(replacement.path, path);
        drop((replacement, other_key, other_env));
        assert!(configs.0.lock_or_panic().is_empty());
    }
}
