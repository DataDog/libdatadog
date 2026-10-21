// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::shm_remote_config::cdn::{CdnHandle, CdnPublisher};
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
    pub(crate) handle: CdnHandle,
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
        let (publisher, handle) = CdnPublisher::new()?;
        let mut fetcher =
            CdnFetcher::new(config.clone(), publisher.clone()).map_err(io::Error::other)?;
        let cancellation = CancellationToken::new();
        let subscription = Arc::new(CdnSubscription {
            config: config.clone(),
            handle,
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
        assert!(!Arc::ptr_eq(&first, &other_key));
        assert!(!Arc::ptr_eq(&first, &other_env));
        let cancelled = first.cancellation.clone();
        let old_subscription = Arc::downgrade(&first);
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
        assert!(old_subscription.upgrade().is_none());
        drop((replacement, other_key, other_env));
        assert!(configs.0.lock_or_panic().is_empty());
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::shm_remote_config::cdn::{CdnDeliveryState, CdnReader};
    use libdd_ffe::cdn::CdnEndpoint;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    #[cfg_attr(miri, ignore = "requires native HTTP and shared memory")]
    fn cdn_shutdown_before_runtime_drop_reports_stopped() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (configs, subscription, mut reader, initial) = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let config = CdnConfig {
                endpoint: CdnEndpoint::custom(&format!("http://{}/config", listener.local_addr().unwrap())).unwrap(),
                language: "php".into(), version: "test".into(),
                poll_interval: Duration::from_secs(30), request_timeout: Duration::from_secs(5),
            };
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = [0; 4096];
                assert!(stream.read(&mut buf).await.unwrap() > 0);
                let body = r#"{"data":{"type":"universal-flag-configuration","attributes":{"format":"SERVER","createdAt":"2026-10-14T00:00:00Z","environment":{"name":"test"},"flags":{}}}}"#;
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            });
            let configs = CdnConfigs::default();
            let subscription = configs.subscribe(config).unwrap();
            let mut reader = CdnReader::new(subscription.handle.clone()).unwrap();
            let initial = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(snapshot) = reader.read().unwrap() {
                        assert_eq!(snapshot.state, CdnDeliveryState::Ready);
                        break snapshot;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            }).await.unwrap();
            (configs, subscription, reader, initial)
        });
        configs.shutdown();
        drop(runtime);
        let terminal = reader.read().unwrap().unwrap();
        assert_eq!(terminal.configuration, initial.configuration);
        assert_eq!(terminal.revision, initial.revision);
        assert_eq!(terminal.state, CdnDeliveryState::Stopped);
        drop(subscription);
    }
}
