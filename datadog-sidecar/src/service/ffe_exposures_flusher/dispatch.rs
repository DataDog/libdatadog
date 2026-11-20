// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::super::evp_transport::EvpTransport;
use super::{
    ADMISSION_QUEUE_CAPACITY, COALESCE_WINDOW, DEFERRED_BYTES, DEFERRED_SLOTS, Job, MAX_BATCH_ROWS,
};
use crate::service::FfeExposureBatch;
use futures::{StreamExt, stream::FuturesUnordered};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

const WRITERS: usize = 8;

#[derive(Default, Clone, Copy, PartialEq, Eq)]
struct Usage {
    singles: usize,
    chunks: usize,
    bytes: usize,
}

impl Usage {
    fn add(&mut self, bytes: Option<usize>) {
        match bytes {
            Some(n) => {
                self.chunks += 1;
                self.bytes += n;
            }
            None => self.singles += 1,
        }
    }
    fn release(&mut self, bytes: Option<usize>) {
        match bytes {
            Some(n) => {
                self.chunks -= 1;
                self.bytes -= n;
            }
            None => self.singles -= 1,
        }
    }
    fn fits(&self, bytes: Option<usize>, singles: usize, chunks: usize, byte_limit: usize) -> bool {
        match bytes {
            Some(n) => self.chunks < chunks && n <= byte_limit.saturating_sub(self.bytes),
            None => self.singles < singles,
        }
    }
}

#[derive(Default)]
struct Ledger {
    total: Usage,
    routes: HashMap<Arc<str>, Usage>,
}

#[derive(Clone)]
pub(super) struct Capacity {
    state: Arc<Mutex<Ledger>>,
    singles: usize,
}

impl Default for Capacity {
    fn default() -> Self {
        Self::new(ADMISSION_QUEUE_CAPACITY)
    }
}

impl Capacity {
    pub(super) fn new(singles: usize) -> Self {
        Self {
            state: Default::default(),
            singles,
        }
    }
    pub(super) fn reserve(&self, key: Arc<str>, bytes: Option<usize>) -> Option<Credit> {
        let mut ledger = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !ledger
            .total
            .fits(bytes, 2 * self.singles, 2 * DEFERRED_SLOTS, DEFERRED_BYTES)
        {
            return None;
        }
        let usage = ledger.routes.get(&key).copied().unwrap_or_default();
        if !usage.fits(bytes, self.singles, DEFERRED_SLOTS, DEFERRED_BYTES / 2) {
            return None;
        }
        ledger.routes.entry(key.clone()).or_default().add(bytes);
        ledger.total.add(bytes);
        Some(Credit {
            state: self.state.clone(),
            key,
            bytes,
        })
    }
}

#[test]
fn quotas_hold_until_delivery_and_release_idle_routes() {
    let capacity = Capacity::new(1);
    let a: Arc<str> = "a".into();
    let b: Arc<str> = "b".into();
    let one = capacity.reserve(a.clone(), None).unwrap();
    assert!(capacity.reserve(a.clone(), None).is_none());
    let two = capacity.reserve(b.clone(), None).unwrap();
    assert!(capacity.reserve("c".into(), None).is_none());
    let chunk_a = capacity
        .reserve(a.clone(), Some(DEFERRED_BYTES / 2))
        .unwrap();
    assert!(capacity.reserve(a, Some(1)).is_none());
    let chunk_b = capacity.reserve(b, Some(DEFERRED_BYTES / 2)).unwrap();
    assert!(capacity.reserve("c".into(), Some(1)).is_none());
    drop((one, two, chunk_a, chunk_b));
    assert!(capacity.state.lock().unwrap().routes.is_empty());
}

pub(in crate::service) struct Credit {
    state: Arc<Mutex<Ledger>>,
    key: Arc<str>,
    bytes: Option<usize>,
}

impl Drop for Credit {
    fn drop(&mut self) {
        let mut ledger = self.state.lock().unwrap_or_else(|e| e.into_inner());
        ledger.total.release(self.bytes);
        if let Some(usage) = ledger.routes.get_mut(&self.key) {
            usage.release(self.bytes);
            if *usage == Usage::default() {
                ledger.routes.remove(&self.key);
            }
        }
    }
}

type Work = (EvpTransport, FfeExposureBatch, Vec<Credit>);

pub(super) async fn dispatch<F, Fut>(mut rx: mpsc::Receiver<Job>, send: F)
where
    F: Fn(EvpTransport, FfeExposureBatch) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut lanes: HashMap<Arc<str>, VecDeque<Work>> = HashMap::new();
    let mut ready = VecDeque::new();
    let mut pending = FuturesUnordered::new();
    let mut jobs = Vec::with_capacity(MAX_BATCH_ROWS);
    let mut closed = false;
    loop {
        while pending.len() < WRITERS {
            let Some(key) = ready.pop_front() else { break };
            let Some((route, batch, credits)) = lanes.get_mut(&key).and_then(VecDeque::pop_front)
            else {
                continue;
            };
            let write = send(route, batch);
            pending.push(async move {
                write.await;
                drop(credits);
                key
            });
        }
        if closed && pending.is_empty() {
            return;
        }
        tokio::select! {
            biased;
            Some(key) = pending.next(), if !pending.is_empty() => {
                if lanes.get(&key).is_some_and(VecDeque::is_empty) { lanes.remove(&key); } else { ready.push_back(key); }
            },
            n = rx.recv_many(&mut jobs, MAX_BATCH_ROWS), if !closed => {
                if n == 0 { closed = true; continue; }
                if n < MAX_BATCH_ROWS && !rx.is_closed() {
                    tokio::time::sleep(COALESCE_WINDOW).await;
                    jobs.extend(std::iter::from_fn(|| rx.try_recv().ok()).take(MAX_BATCH_ROWS-n));
                }
                let mut groups = HashMap::new();
                let mut batches: Vec<Work> = Vec::new();
                for (route, mut batch, credit) in jobs.drain(..) {
                    // A deferred chunk already occupies its own wire-sized batch.
                    let chunk = credit.as_ref().is_some_and(|c| c.bytes.is_some());
                    let group = (route.scope_handle(), batch.context.clone());
                    if !chunk && let Some(&i) = groups.get(&group) {
                        let existing: &mut Work = &mut batches[i];
                        existing.1.exposures.append(&mut batch.exposures);
                        existing.2.extend(credit);
                    } else {
                        if chunk { groups.remove(&group); } else { groups.insert(group, batches.len()); }
                        batches.push((route,batch,credit.into_iter().collect()));
                    }
                }
                for work in batches {
                    let key = work.0.scope_handle();
                    lanes.entry(key.clone()).or_insert_with(|| { ready.push_back(key); VecDeque::new() }).push_back(work);
                }
            }
        }
    }
}
