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
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Connections own subscriptions. The cache only holds weak references, so the
/// last disconnect cancels polling and releases the configuration/credentials.
#[derive(Clone, Default)]
pub(crate) struct CdnConfigs {
    subscriptions: Arc<Mutex<HashMap<CdnRegistration, Weak<CdnSubscription>>>>,
    shutdown: CancellationToken,
}

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
        let mut subscriptions = self.subscriptions.lock_or_panic();
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
        let cancellation = self.shutdown.child_token();
        let subscription = Arc::new(CdnSubscription {
            config: config.clone(),
            publisher: publisher.clone(),
            cancellation: cancellation.clone(),
            cache: Arc::downgrade(&self.subscriptions),
        });
        subscriptions.insert(config, Arc::downgrade(&subscription));
        tokio::spawn(async move {
            fetcher.run(cancellation).await;
        });
        Ok(subscription)
    }

    pub(crate) fn shutdown(&self) {
        self.shutdown.cancel();
        let subscriptions: Vec<_> = self.subscriptions.lock_or_panic().drain().collect();
        for (_, subscription) in subscriptions {
            if let Some(subscription) = subscription.upgrade() {
                subscription.cancellation.cancel();
            }
        }
    }
}

struct ActiveCdnConnection {
    _subscription: Arc<CdnSubscription>,
    notification: NotificationGuard,
}

#[derive(Default)]
struct ConnectionState {
    active: Option<ActiveCdnConnection>,
    generation: u64,
    closed: bool,
}

/// A one-way registration remains desired even if its initial publication cannot
/// be created. The connection owns both the retry and the eventual subscription.
pub(crate) struct CdnConnection {
    pub(crate) config: CdnRegistration,
    state: Arc<Mutex<ConnectionState>>,
    retry: Option<tokio::task::JoinHandle<()>>,
}

impl CdnConnection {
    pub(crate) fn new(
        configs: CdnConfigs,
        publications: PublicationStore,
        config: CdnRegistration,
        notify: Arc<dyn Fn() + Send + Sync>,
        generation: u64,
    ) -> Result<Self, CdnSubscriptionError> {
        let mut state = ConnectionState {
            generation,
            ..Default::default()
        };
        match configs.subscribe(config.clone(), &publications) {
            Ok(subscription) => state.install(subscription, notify.clone()),
            Err(CdnSubscriptionError::PublicationUnavailable) => {}
            Err(error) => return Err(error),
        }
        let state = Arc::new(Mutex::new(state));
        let retry = if state.lock_or_panic().active.is_none() {
            let state = state.clone();
            let config = config.clone();
            Some(tokio::spawn(async move {
                // Only resource/publication failures are retried. Invalid inputs
                // remain terminal. Cap the delay without blocking the IPC handler.
                let mut delay = Duration::from_millis(100);
                loop {
                    tokio::select! {
                        biased;
                        _ = configs.shutdown.cancelled() => return,
                        _ = tokio::time::sleep(delay) => {}
                    }
                    let mut state = state.lock_or_panic();
                    if state.closed || configs.shutdown.is_cancelled() {
                        return;
                    }
                    match configs.subscribe(config.clone(), &publications) {
                        Ok(subscription) => {
                            state.install(subscription, notify);
                            return;
                        }
                        Err(CdnSubscriptionError::PublicationUnavailable) => {}
                        Err(error) => {
                            tracing::warn!("Could not register CDN configuration: {error}");
                            return;
                        }
                    }
                    delay = (delay * 2).min(Duration::from_secs(5));
                }
            }))
        } else {
            None
        };
        Ok(Self {
            config,
            state,
            retry,
        })
    }

    pub(crate) fn notify_current(&self, generation: u64) {
        let mut state = self.state.lock_or_panic();
        state.generation = generation;
        if let Some(active) = &state.active {
            active.notification.notify_current(generation);
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn subscription(&self) -> Option<Arc<CdnSubscription>> {
        self.state
            .lock_or_panic()
            .active
            .as_ref()
            .map(|a| a._subscription.clone())
    }
}

impl ConnectionState {
    fn install(&mut self, subscription: Arc<CdnSubscription>, notify: Arc<dyn Fn() + Send + Sync>) {
        let notification = subscription.publisher.register(notify, self.generation);
        self.active = Some(ActiveCdnConnection {
            _subscription: subscription,
            notification,
        });
    }
}

impl Drop for CdnConnection {
    fn drop(&mut self) {
        // Serialize with a retry already creating its publication, then release
        // that subscription before replacement/None/disconnect returns.
        let mut state = self.state.lock_or_panic();
        state.closed = true;
        state.active = None;
        if let Some(retry) = self.retry.take() {
            retry.abort();
        }
    }
}
