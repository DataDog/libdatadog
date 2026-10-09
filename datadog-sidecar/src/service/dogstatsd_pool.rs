// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Process-wide DogStatsD clients, shared by all connections using the same endpoint.
//!
//! A client queues metrics into a lock-free bounded channel drained by its own worker, so
//! connections share it without contention. Connections keep their client, so sending never
//! touches the pool lock. The pool refers to the clients only weakly: the sidecar is long-lived,
//! so a client, with its worker and socket, ends with the last connection using its endpoint.

use libdd_common::{Endpoint, MutexExt};
use libdd_dogstatsd_client::DogStatsDClient;
use std::sync::{Arc, Mutex, Weak};
use zwohash::HashMap;

#[derive(Default, Clone)]
pub(crate) struct DogStatsDPool(Arc<Mutex<HashMap<String, Weak<DogStatsDClient>>>>);

impl DogStatsDPool {
    pub(crate) fn client_for(&self, endpoint: &Endpoint) -> Option<Arc<DogStatsDClient>> {
        // The client does not support test tokens, so these must not split the pool.
        let key = endpoint.url.to_string();
        let mut pool = self.0.lock_or_panic();
        if let Some(client) = pool.get(&key).and_then(Weak::upgrade) {
            return Some(client);
        }
        let client = Arc::new(DogStatsDClient::new(endpoint.clone()).ok()?);
        pool.retain(|_, client| client.strong_count() > 0);
        pool.insert(key, Arc::downgrade(&client));
        Some(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connections_share_the_client_of_an_endpoint_while_it_is_used() {
        let pool = DogStatsDPool::default();
        let endpoint = Endpoint::from_slice("udp://localhost:8125");
        let mut token_endpoint = endpoint.clone();
        token_endpoint.test_token = Some("token".into());
        let client = pool.client_for(&endpoint).unwrap();
        assert!(Arc::ptr_eq(
            &client,
            &pool.client_for(&token_endpoint).unwrap()
        ));
        let other = pool
            .client_for(&Endpoint::from_slice("udp://localhost:8126"))
            .unwrap();
        assert!(!Arc::ptr_eq(&client, &other));
        assert_eq!(pool.0.lock_or_panic().len(), 2);

        // An unused client ends; its endpoint is forgotten once another client is created.
        let unused = Arc::downgrade(&client);
        drop(client);
        assert!(unused.upgrade().is_none());
        let _third = pool
            .client_for(&Endpoint::from_slice("udp://localhost:8127"))
            .unwrap();
        assert_eq!(pool.0.lock_or_panic().len(), 2);
    }
}
