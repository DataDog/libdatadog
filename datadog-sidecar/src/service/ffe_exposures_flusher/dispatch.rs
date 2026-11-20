// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::super::evp_transport::EvpTransport;
use super::{
    ADMISSION_QUEUE_CAPACITY, COALESCE_WINDOW, DEFERRED_BYTES, DEFERRED_ROWS, Job, MAX_BATCH_ROWS,
};
use crate::service::{FfeExposure, FfeExposureBatch};
use futures::{StreamExt, stream::FuturesUnordered};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::hash::{BuildHasher, RandomState};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

const WRITERS: usize = 16;
pub(super) const ROUTE_WRITERS: usize = 4;
const HTTP_BATCH_ROWS: usize = 512;

#[derive(Clone, Copy)]
pub(super) struct DeferredCharge {
    rows: usize,
    pub(super) bytes: usize,
}

impl DeferredCharge {
    pub(super) fn for_batch(batch: &FfeExposureBatch) -> Self {
        let ctx = &batch.context;
        let rows = batch.exposures.len();
        let mut bytes = ctx.service.capacity()
            + ctx.env.capacity()
            + ctx.version.capacity()
            + size_of::<Job>()
            + size_of::<Credit>()
            + size_of::<Work>()
            + batch.exposures.capacity() * size_of::<FfeExposure>();
        for row in &batch.exposures {
            // Allow for the packed row-vector's spare capacity and the in-flight key index.
            bytes += size_of::<FfeExposure>()
                + 32
                + row.flag_key.capacity()
                + row.subject_id.capacity()
                + row.variant.capacity()
                + row.allocation_key.capacity()
                + row.features_json.capacity()
                + row.subject_attributes_json.capacity();
        }
        Self { rows, bytes }
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
struct Usage {
    singles: usize,
    rows: usize,
    bytes: usize,
}

impl Usage {
    fn add(&mut self, charge: Option<DeferredCharge>) {
        if let Some(c) = charge {
            self.rows += c.rows;
            self.bytes += c.bytes;
        } else {
            self.singles += 1;
        }
    }
    fn release(&mut self, charge: Option<DeferredCharge>) {
        if let Some(c) = charge {
            self.rows -= c.rows;
            self.bytes -= c.bytes;
        } else {
            self.singles -= 1;
        }
    }
    fn fits(
        &self,
        charge: Option<DeferredCharge>,
        singles: usize,
        rows: usize,
        bytes: usize,
    ) -> bool {
        match charge {
            Some(c) => c.rows > 0 && c.rows <= rows - self.rows && c.bytes <= bytes - self.bytes,
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
    pub(super) fn outstanding_rows(&self) -> usize {
        let usage = self.state.lock().unwrap_or_else(|e| e.into_inner()).total;
        usage.singles + usage.rows
    }
    pub(super) fn reserve(&self, key: Arc<str>, charge: Option<DeferredCharge>) -> Option<Credit> {
        let mut ledger = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !ledger
            .total
            .fits(charge, 2 * self.singles, 2 * DEFERRED_ROWS, DEFERRED_BYTES)
        {
            return None;
        }
        let usage = ledger.routes.get(&key).copied().unwrap_or_default();
        if !usage.fits(charge, self.singles, DEFERRED_ROWS, DEFERRED_BYTES / 2) {
            return None;
        }
        ledger.routes.entry(key.clone()).or_default().add(charge);
        ledger.total.add(charge);
        Some(Credit {
            state: self.state.clone(),
            key,
            charge,
        })
    }
}

pub(in crate::service) struct Credit {
    state: Arc<Mutex<Ledger>>,
    key: Arc<str>,
    charge: Option<DeferredCharge>,
}
impl Drop for Credit {
    fn drop(&mut self) {
        let mut ledger = self.state.lock().unwrap_or_else(|e| e.into_inner());
        ledger.total.release(self.charge);
        if let Some(usage) = ledger.routes.get_mut(&self.key) {
            usage.release(self.charge);
            if *usage == Usage::default() {
                ledger.routes.remove(&self.key);
            }
        }
    }
}

struct Work {
    route: EvpTransport,
    batch: FfeExposureBatch,
    credits: Vec<Credit>,
    keys: Vec<u64>,
}
#[derive(Default)]
struct Lane {
    queue: VecDeque<Work>,
    in_flight: usize,
    active_keys: HashSet<u64>,
    ready: bool,
}
impl Lane {
    fn push(&mut self, job: Job, hashes: &RandomState) {
        let (route, mut batch, credit) = job;
        // A hash collision serializes two unrelated keys. Assignment equality stays in the LRU.
        let keys = batch
            .exposures
            .iter()
            .map(|e| hashes.hash_one((&batch.context, &e.flag_key, &e.subject_id)));
        if let Some(tail) = self.queue.back_mut()
            && tail.batch.context == batch.context
            && tail.batch.exposures.len() + batch.exposures.len() <= HTTP_BATCH_ROWS
        {
            tail.keys.extend(keys);
            tail.batch.exposures.append(&mut batch.exposures);
            tail.credits.extend(credit);
        } else {
            let keys = keys.collect();
            self.queue.push_back(Work {
                route,
                batch,
                credits: credit.into_iter().collect(),
                keys,
            });
        }
    }
    fn can_start(&self) -> bool {
        self.in_flight < ROUTE_WRITERS
            && self
                .queue
                .front()
                .is_some_and(|w| w.keys.iter().all(|k| !self.active_keys.contains(k)))
    }

    fn schedule(&mut self, key: &Arc<str>, ready: &mut VecDeque<Arc<str>>) {
        if !self.ready && self.can_start() {
            self.ready = true;
            ready.push_back(key.clone());
        }
    }
}

pub(super) async fn dispatch<F, Fut>(rx: mpsc::Receiver<Job>, send: F)
where
    F: Fn(EvpTransport, FfeExposureBatch) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut lanes: HashMap<Arc<str>, Lane> = HashMap::new();
    let hashes = RandomState::new();
    let mut ready = VecDeque::new();
    let mut pending = FuturesUnordered::new();
    let bursts = futures::stream::unfold(rx, |mut rx| async move {
        let mut jobs = Vec::with_capacity(MAX_BATCH_ROWS);
        let n = rx.recv_many(&mut jobs, MAX_BATCH_ROWS).await;
        if n == 0 {
            return None;
        }
        if n < MAX_BATCH_ROWS && !rx.is_closed() {
            tokio::time::sleep(COALESCE_WINDOW).await;
            jobs.extend(std::iter::from_fn(|| rx.try_recv().ok()).take(MAX_BATCH_ROWS - n));
        }
        Some((jobs, rx))
    });
    tokio::pin!(bursts);
    let mut closed = false;
    loop {
        while pending.len() < WRITERS {
            let Some(key) = ready.pop_front() else { break };
            let Some(lane) = lanes.get_mut(&key) else {
                continue;
            };
            lane.ready = false;
            // A tail-merge can add a conflicting key while this lane waits for a global writer.
            if !lane.can_start() {
                continue;
            }
            let Some(work) = lane.queue.pop_front() else {
                continue;
            };
            lane.in_flight += 1;
            lane.active_keys.extend(work.keys.iter().copied());
            lane.schedule(&key, &mut ready);
            let write = send(work.route, work.batch);
            pending.push(async move {
                write.await;
                drop(work.credits);
                (key, work.keys)
            });
        }
        if closed && pending.is_empty() {
            return;
        }
        tokio::select! {
            biased;
            Some((key, keys)) = pending.next(), if !pending.is_empty() => {
                if let Some(lane) = lanes.get_mut(&key) {
                    lane.in_flight -= 1;
                    for k in keys { lane.active_keys.remove(&k); }
                    if lane.queue.is_empty() && lane.in_flight == 0 { lanes.remove(&key); }
                    else { lane.schedule(&key, &mut ready); }
                }
            },
            burst = bursts.next(), if !closed => {
                let Some(jobs) = burst else { closed = true; continue; };
                for job in jobs {
                    let key = job.0.scope_handle();
                    let lane = lanes.entry(key.clone()).or_default();
                    lane.push(job, &hashes);
                    lane.schedule(&key, &mut ready);
                }
            }
        }
    }
}

#[test]
fn quotas_hold_until_delivery_and_release_idle_routes() {
    let cap = Capacity::new(1);
    let a: Arc<str> = "a".into();
    let b: Arc<str> = "b".into();
    let x = cap.reserve(a.clone(), None).unwrap();
    assert!(cap.reserve(a.clone(), None).is_none());
    let y = cap.reserve(b.clone(), None).unwrap();
    assert!(cap.reserve("c".into(), None).is_none());
    let charge = |rows, bytes| Some(DeferredCharge { rows, bytes });
    let z = cap.reserve(a.clone(), charge(DEFERRED_ROWS, 1)).unwrap();
    assert!(cap.reserve(a, charge(1, 1)).is_none());
    let w = cap
        .reserve(b.clone(), charge(1, DEFERRED_BYTES / 2))
        .unwrap();
    assert!(cap.reserve(b, charge(1, 1)).is_none());
    let last = cap
        .reserve("c".into(), charge(1, DEFERRED_BYTES / 2 - 1))
        .unwrap();
    assert!(cap.reserve("d".into(), charge(1, 1)).is_none());
    drop((x, y, z, w, last));
    assert_eq!(cap.outstanding_rows(), 0);
    assert!(cap.state.lock().unwrap().routes.is_empty());
}
