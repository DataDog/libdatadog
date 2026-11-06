// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::service::sidecar_interface::CdnSubscriptionError;
use crate::shm_remote_config::{
    CdnRegistration,
    publication::{CdnPublisher, NotificationGuard, PublicationStore},
};
use libdd_common::MutexExt;
use libdd_ffe::cdn::CdnFetcher;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use tokio_util::sync::CancellationToken;

/// Connections own subscriptions. The cache only holds weak references, so the
/// last disconnect cancels polling and releases the configuration/credentials.
#[derive(Clone, Default)]
pub(crate) struct CdnConfigs(Arc<Mutex<HashMap<CdnRegistration, Weak<CdnSubscription>>>>);

pub(crate) struct CdnSubscription {
    pub(crate) config: CdnRegistration,
    pub(crate) publisher: CdnPublisher,
    cancellation: CancellationToken,
    cache: Weak<Mutex<HashMap<CdnRegistration, Weak<CdnSubscription>>>>,
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
    pub(crate) fn subscribe(
        &self,
        config: CdnRegistration,
        publications: &PublicationStore,
    ) -> Result<Arc<CdnSubscription>, CdnSubscriptionError> {
        config.config.validate()?;
        let mut subscriptions = self.0.lock_or_panic();
        // Expired keys own credentials too; remove them before admitting work.
        subscriptions.retain(|_, value| value.strong_count() != 0);
        if let Some(subscription) = subscriptions.get(&config).and_then(Weak::upgrade) {
            return Ok(subscription);
        }
        let publisher = publications.cdn(&config).map_err(|error| {
            tracing::warn!("Could not create CDN remote configuration publication: {error}");
            CdnSubscriptionError::PublicationUnavailable
        })?;
        let mut fetcher = CdnFetcher::new(config.config.clone(), publisher.clone())?;
        let cancellation = CancellationToken::new();
        let subscription = Arc::new(CdnSubscription {
            config: config.clone(),
            publisher: publisher.clone(),
            cancellation: cancellation.clone(),
            cache: Arc::downgrade(&self.0),
        });
        subscriptions.insert(config, Arc::downgrade(&subscription));
        tokio::spawn(async move {
            fetcher.run(cancellation).await;
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

pub(crate) struct CdnConnection {
    pub(crate) subscription: Arc<CdnSubscription>,
    pub(crate) _notification: Option<NotificationGuard>,
}
