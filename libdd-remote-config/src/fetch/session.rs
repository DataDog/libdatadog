// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::{ConfigApplyState, ConfigOptions, FileStorage, SingleFetcher};
use crate::{RemoteConfigCapabilities, RemoteConfigPath, RemoteConfigProduct, Target};
use hashbrown::HashMap;
use libdd_capabilities::{Bytes, HttpClientCapability, SleepCapability};
use libdd_trace_protobuf::remoteconfig::TargetFileHash;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

struct Revision {
    path: Arc<RemoteConfigPath>,
    version: u64,
    bytes: Bytes,
}

struct VersionedStorage;

impl FileStorage for VersionedStorage {
    type StoredFile = Revision;

    fn update_in_place(&self) -> bool {
        false
    }

    fn acknowledge_on_receipt(&self) -> bool {
        false
    }

    fn store(
        &self,
        version: u64,
        path: Arc<RemoteConfigPath>,
        bytes: Vec<u8>,
    ) -> anyhow::Result<Arc<Revision>> {
        Ok(Arc::new(Revision {
            path,
            version,
            bytes: Bytes::from(bytes),
        }))
    }

    fn update(&self, _: &Arc<Revision>, _: u64, _: Vec<u8>) -> anyhow::Result<()> {
        anyhow::bail!("cannot overwrite a retained remote configuration revision")
    }

    fn reissue_cached(&self, file: &Arc<Revision>, version: u64) -> Option<Arc<Revision>> {
        Some(Arc::new(Revision {
            path: file.path.clone(),
            version,
            bytes: file.bytes.clone(),
        }))
    }
}

/// A verified revision assigned to this session. Retaining one never copies its payload.
pub struct AssignedFile {
    revision: Arc<Revision>,
    hashes: Vec<TargetFileHash>,
    assigned_at: u64,
    response_position: usize,
}

impl AssignedFile {
    pub fn path(&self) -> &RemoteConfigPath {
        &self.revision.path
    }

    pub fn version(&self) -> u64 {
        self.revision.version
    }

    pub fn bytes(&self) -> &[u8] {
        &self.revision.bytes
    }

    pub fn hashes(&self) -> &[TargetFileHash] {
        &self.hashes
    }
}

/// The complete, authoritative assignment at an accepted delivery generation.
pub struct Assignment {
    pub generation: u64,
    pub target_generation: u64,
    files: Vec<Arc<AssignedFile>>,
    positions: HashMap<Arc<RemoteConfigPath>, usize>,
}

impl Assignment {
    fn new(generation: u64, target_generation: u64, files: Vec<Arc<AssignedFile>>) -> Self {
        let positions = files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.revision.path.clone(), index))
            .collect();
        Self {
            generation,
            target_generation,
            files,
            positions,
        }
    }

    pub fn files(&self) -> impl ExactSizeIterator<Item = &Arc<AssignedFile>> {
        self.files.iter()
    }

    fn get(&self, path: &RemoteConfigPath) -> Option<&Arc<AssignedFile>> {
        self.positions.get(path).map(|&index| &self.files[index])
    }
}

#[derive(Clone)]
pub enum AssignmentChange {
    Upsert {
        file: Arc<AssignedFile>,
        previous: Option<Arc<AssignedFile>>,
    },
    Remove(Arc<AssignedFile>),
}

impl AssignmentChange {
    fn file(&self) -> &AssignedFile {
        match self {
            Self::Upsert { file, .. } | Self::Remove(file) => file,
        }
    }
}

pub struct AssignmentBatch {
    pub changes: Vec<AssignmentChange>,
    pub desired: Arc<Assignment>,
}

pub struct SessionPoll {
    pub result: anyhow::Result<Option<AssignmentBatch>>,
    pub next_poll_after: Duration,
}

const MAX_WATCH_PENDING: usize = 32;
const MAX_WATCH_QUEUED_BYTES: usize = 32 * 1024 * 1024;
const MAX_WATCH_DIAGNOSTIC_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchMode {
    LatestState,
    OrderedState { max_pending: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum WatchReason {
    Initial,
    Changed,
    Replay,
    Retry,
}

#[derive(Debug, thiserror::Error)]
#[error("ordered watch overflow; reopen the watch or retarget the session")]
pub struct WatchOverflow;

#[derive(Clone, Copy)]
pub enum WatchOutcome<'text> {
    Applied,
    Retained,
    Failed(&'text str),
}

impl<'text> WatchOutcome<'text> {
    pub fn failure(self) -> Option<&'text str> {
        match self {
            Self::Failed(error) => Some(error),
            Self::Applied | Self::Retained => None,
        }
    }
}

pub enum WatchDecision<'text> {
    Eligible,
    Ignored,
    Rejected(&'text str),
}

impl<'text> WatchDecision<'text> {
    pub fn diagnostic(&self, failure: Option<&'text str>) -> Option<&'text str> {
        match self {
            Self::Eligible => failure,
            Self::Ignored => None,
            Self::Rejected(error) => Some(error),
        }
    }
}

#[derive(Clone)]
pub struct ScopedAssignment {
    assignment: Arc<Assignment>,
}

impl ScopedAssignment {
    fn new(assignment: &Arc<Assignment>, products: &[RemoteConfigProduct]) -> Self {
        let owns = |file: &Arc<AssignedFile>| products.contains(&file.path().product());
        let assignment = match assignment.files().position(|file| !owns(file)) {
            Some(first_unowned) => {
                let files = assignment.files[..first_unowned]
                    .iter()
                    .chain(
                        assignment.files[first_unowned + 1..]
                            .iter()
                            .filter(|file| owns(file)),
                    )
                    .cloned()
                    .collect();
                Arc::new(Assignment::new(
                    assignment.generation,
                    assignment.target_generation,
                    files,
                ))
            }
            None => assignment.clone(),
        };
        Self { assignment }
    }

    pub fn generation(&self) -> u64 {
        self.assignment.generation
    }

    pub fn target_generation(&self) -> u64 {
        self.assignment.target_generation
    }

    pub fn files(&self) -> impl Iterator<Item = &Arc<AssignedFile>> {
        self.assignment.files()
    }
}

#[derive(Clone)]
pub struct WatchUpdate {
    pub ticket: i32,
    pub reason: WatchReason,
    pub desired: ScopedAssignment,
    changes: Arc<[AssignmentChange]>,
}

impl WatchUpdate {
    pub fn target_generation(&self) -> u64 {
        self.desired.target_generation()
    }

    /// Upserts follow response order across accepted deliveries. Removals follow upserts.
    pub fn changes(&self) -> &[AssignmentChange] {
        &self.changes
    }
}

struct WatchPending {
    reason: WatchReason,
    assignment: ScopedAssignment,
    bytes: usize,
}

struct Watch {
    products: Vec<RemoteConfigProduct>,
    mode: WatchMode,
    pending: VecDeque<WatchPending>,
    queued_bytes: usize,
    overflow: bool,
    ready: bool,
    closing: bool,
    ticket: i32,
    running: Option<WatchUpdate>,
    evaluated: Option<Arc<Assignment>>,
    retry: Option<WatchPending>,
}

impl Watch {
    fn owns(&self, file: &AssignedFile) -> bool {
        self.products.contains(&file.path().product())
    }

    fn enqueue_assignment(&mut self, reason: WatchReason, assignment: &Arc<Assignment>) {
        self.enqueue(WatchPending {
            reason,
            assignment: ScopedAssignment::new(assignment, &self.products),
            bytes: 0,
        });
    }

    fn enqueue(&mut self, mut update: WatchPending) {
        match self.mode {
            WatchMode::LatestState => {
                if let Some(previous) = self.pending.pop_back()
                    && (previous.reason == WatchReason::Initial
                        || (previous.reason == WatchReason::Replay
                            && update.reason == WatchReason::Changed))
                {
                    update.reason = previous.reason;
                }
            }
            WatchMode::OrderedState { max_pending } => {
                if self.pending.len() < max_pending {
                    update.bytes = update.assignment.files().fold(0usize, |bytes, file| {
                        bytes.saturating_add(file.bytes().len())
                    });
                }
                if self.pending.len() == max_pending
                    || update.bytes > MAX_WATCH_QUEUED_BYTES - self.queued_bytes
                {
                    self.clear_queue();
                    self.overflow = true;
                    return;
                }
                self.queued_bytes += update.bytes;
            }
        }
        self.pending.push_back(update);
    }

    fn clear_queue(&mut self) {
        self.pending.clear();
        self.queued_bytes = 0;
        self.retry = None;
    }

    fn discard_retry(&mut self) {
        self.retry = None;
        if self
            .pending
            .front()
            .is_some_and(|update| update.reason == WatchReason::Retry)
            && let Some(retry) = self.pending.pop_front()
        {
            self.queued_bytes -= retry.bytes;
        }
    }

    fn observe(&mut self, batch: Option<&AssignmentBatch>) {
        if self.closing || self.overflow {
            return;
        }
        if let Some(batch) = batch
            && (!self.ready || batch.changes.iter().any(|change| self.owns(change.file())))
        {
            let reason = if self.ready {
                WatchReason::Changed
            } else {
                WatchReason::Initial
            };
            self.ready = true;
            self.discard_retry();
            self.enqueue_assignment(reason, &batch.desired);
        } else if let Some(retry) = self.retry.take()
            && self.pending.is_empty()
        {
            self.enqueue(retry);
        }
    }

    fn next(
        &mut self,
        reserve: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<WatchUpdate>> {
        if self.overflow {
            return Err(WatchOverflow.into());
        }
        if self.running.is_some() || self.pending.is_empty() || self.closing {
            return Ok(None);
        }
        let ticket = self
            .ticket
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("watch tickets exhausted"))?;
        reserve()?;
        let pending = self
            .pending
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("missing watch update"))?;
        self.ticket = ticket;
        self.queued_bytes -= pending.bytes;
        let changes = self.changes(&pending.assignment.assignment);
        let update = WatchUpdate {
            ticket,
            reason: pending.reason,
            desired: pending.assignment,
            changes,
        };
        self.running = Some(update.clone());
        Ok(Some(update))
    }

    fn changes(&self, assignment: &Assignment) -> Arc<[AssignmentChange]> {
        let previous = self.evaluated.as_deref();
        let mut changes = Vec::new();
        for file in assignment.files() {
            let old = previous.and_then(|old| old.get(file.path()));
            if !old.is_some_and(|old| Arc::ptr_eq(old, file)) {
                changes.push(AssignmentChange::Upsert {
                    file: file.clone(),
                    previous: old.cloned(),
                });
            }
        }
        changes.sort_unstable_by_key(|change| {
            let file = change.file();
            (file.assigned_at, file.response_position)
        });
        if let Some(previous) = previous {
            for file in previous.files() {
                if assignment.get(file.path()).is_none() {
                    changes.push(AssignmentChange::Remove(file.clone()));
                }
            }
        }
        changes.into()
    }
}

/// One application-aware remote configuration client. The host owns scheduling and dispatch.
pub struct Session<C: HttpClientCapability + SleepCapability> {
    fetcher: SingleFetcher<VersionedStorage, C>,
    desired: Option<Arc<Assignment>>,
    generation: u64,
    target_generation: u64,
    watches: WatchRegistry,
}

impl<C: HttpClientCapability + SleepCapability> Session<C> {
    pub async fn open(
        target: Target,
        runtime_id: String,
        client_id: String,
        options: ConfigOptions,
        transport: C,
    ) -> anyhow::Result<Self> {
        let mut fetcher = SingleFetcher::new_with_agent_path_prefix(
            VersionedStorage,
            target,
            runtime_id,
            options,
            transport,
            true,
        )
        .await?
        .with_client_id(client_id);
        fetcher.fetcher.retain_agent_assignment();
        Ok(Self {
            fetcher,
            desired: None,
            generation: 0,
            target_generation: 1,
            watches: WatchRegistry::default(),
        })
    }

    pub fn set_interest(
        &mut self,
        products: Vec<RemoteConfigProduct>,
        capabilities: Vec<RemoteConfigCapabilities>,
    ) {
        self.fetcher
            .set_product_capabilities(products, capabilities);
    }

    pub fn set_identity(
        &mut self,
        client_id: String,
        runtime_id: String,
        tags: Vec<String>,
        extra_services: Vec<String>,
        process_tags: Option<Vec<String>>,
    ) {
        self.fetcher.set_identity(client_id, runtime_id, tags);
        self.fetcher.set_extra_services(extra_services);
        if let Some(process_tags) = process_tags {
            self.fetcher.set_process_tags(process_tags);
        }
    }

    pub fn set_extra_services(&mut self, services: Vec<String>) {
        self.fetcher.set_extra_services(services);
    }

    pub fn target_generation(&self) -> u64 {
        self.target_generation
    }

    /// Starts a new target generation without discarding verified file bytes or trust state.
    /// This invalidates receipts and readiness; retained views remain readable.
    pub fn retarget(&mut self, target: Target) -> anyhow::Result<u64> {
        let generation = self
            .target_generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("target generation exhausted"))?;
        self.fetcher.retarget(target);
        self.desired = None;
        self.target_generation = generation;
        for watch in self.watches.entries.values_mut() {
            watch.clear_queue();
            watch.evaluated = None;
            watch.ready = false;
            watch.overflow = false;
        }
        Ok(generation)
    }

    /// `None` means this session has not received an authoritative assignment yet.
    pub fn snapshot(&self) -> Option<&Arc<Assignment>> {
        self.desired.as_ref()
    }

    pub fn install_initial_watches(&mut self, pending: &mut WatchRegistry) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.watches.next == 0 && self.generation == 0,
            "session already started"
        );
        self.watches = std::mem::take(pending);
        Ok(())
    }

    pub fn open_watch(
        &mut self,
        products: Vec<RemoteConfigProduct>,
        mode: WatchMode,
    ) -> anyhow::Result<i32> {
        self.watches
            .open_watch(products, mode, self.desired.as_ref())
    }

    /// Reserve host output capacity before consuming a dispatchable watch update.
    pub fn next_watch_with_reservation(
        &mut self,
        id: i32,
        reserve: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<WatchUpdate>> {
        self.watches.next_watch(id, reserve)
    }

    pub fn replay_watch(&mut self, id: i32) -> anyhow::Result<bool> {
        self.watches.replay_watch(id, self.desired.as_ref())
    }

    pub fn close_watch(&mut self, id: i32) -> bool {
        self.watches.close_watch(id)
    }
}

#[derive(Default)]
pub struct WatchRegistry {
    next: i32,
    entries: HashMap<i32, Watch>,
}

impl WatchRegistry {
    pub fn prepare_watch(
        &mut self,
        products: Vec<RemoteConfigProduct>,
        mode: WatchMode,
    ) -> anyhow::Result<i32> {
        self.open_watch(products, mode, None)
    }

    pub fn contains(&self, id: i32) -> bool {
        self.entries.contains_key(&id)
    }

    fn open_watch(
        &mut self,
        products: Vec<RemoteConfigProduct>,
        mode: WatchMode,
        desired: Option<&Arc<Assignment>>,
    ) -> anyhow::Result<i32> {
        anyhow::ensure!(!products.is_empty(), "a watch needs at least one product");
        for (index, product) in products.iter().enumerate() {
            anyhow::ensure!(
                *product != RemoteConfigProduct::AgentTask,
                "Agent tasks require individual receipts"
            );
            anyhow::ensure!(
                !products[..index].contains(product),
                "duplicate watch product"
            );
            anyhow::ensure!(
                !self
                    .entries
                    .values()
                    .any(|watch| watch.products.contains(product)),
                "product already has a watch owner"
            );
        }
        if let WatchMode::OrderedState { max_pending } = mode {
            anyhow::ensure!(
                (1..=MAX_WATCH_PENDING).contains(&max_pending),
                "invalid ordered watch capacity"
            );
        }
        let id = self
            .next
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("watch IDs exhausted"))?;
        let mut watch = Watch {
            products,
            mode,
            pending: VecDeque::new(),
            queued_bytes: 0,
            overflow: false,
            ready: desired.is_some(),
            closing: false,
            ticket: 0,
            running: None,
            evaluated: None,
            retry: None,
        };
        if let Some(assignment) = desired {
            watch.enqueue_assignment(WatchReason::Replay, assignment);
        }
        self.next = id;
        self.entries.insert(id, watch);
        Ok(id)
    }

    fn next_watch(
        &mut self,
        id: i32,
        reserve: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<Option<WatchUpdate>> {
        self.entries
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("invalid or closed watch"))?
            .next(reserve)
    }

    fn replay_watch(&mut self, id: i32, desired: Option<&Arc<Assignment>>) -> anyhow::Result<bool> {
        let watch = self
            .entries
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("invalid or closed watch"))?;
        anyhow::ensure!(!watch.closing, "watch is closing");
        if watch.overflow {
            return Err(WatchOverflow.into());
        }
        let Some(assignment) = desired else {
            return Ok(false);
        };
        watch.discard_retry();
        watch.enqueue_assignment(WatchReason::Replay, assignment);
        if watch.overflow {
            return Err(WatchOverflow.into());
        }
        Ok(true)
    }

    pub fn close_watch(&mut self, id: i32) -> bool {
        if let Some(watch) = self.entries.get_mut(&id)
            && watch.running.is_some()
        {
            watch.clear_queue();
            watch.evaluated = None;
            watch.overflow = false;
            watch.closing = true;
            return false;
        }
        self.entries.remove(&id);
        true
    }
}

impl<C: HttpClientCapability + SleepCapability> Session<C> {
    pub fn finish_watch(
        &mut self,
        id: i32,
        ticket: i32,
        outcome: WatchOutcome<'_>,
        decisions: &[WatchDecision<'_>],
    ) -> anyhow::Result<usize> {
        let watch = self
            .watches
            .entries
            .get_mut(&id)
            .ok_or_else(|| anyhow::anyhow!("invalid or closed watch"))?;
        let update = watch
            .running
            .as_ref()
            .filter(|update| update.ticket == ticket)
            .ok_or_else(|| anyhow::anyhow!("stale watch ticket"))?;
        let close = watch.closing;
        let current_target = update.target_generation() == self.target_generation;
        let failure = outcome.failure();
        if !close && current_target {
            let upsert_count = update
                .changes
                .iter()
                .filter(|change| matches!(change, AssignmentChange::Upsert { .. }))
                .count();
            anyhow::ensure!(
                decisions.len() == upsert_count,
                "a watch decision is required for each upsert"
            );
            let diagnostic_bytes = decisions
                .iter()
                .filter_map(|decision| decision.diagnostic(failure))
                .fold(0usize, |bytes, error| bytes.saturating_add(error.len()));
            anyhow::ensure!(
                diagnostic_bytes <= MAX_WATCH_DIAGNOSTIC_BYTES,
                "watch diagnostic budget exhausted"
            );
        }
        let update = watch
            .running
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing running watch"))?;
        if close {
            self.watches.entries.remove(&id);
            return Ok(0);
        }
        if !current_target {
            return Ok(0);
        }
        if failure.is_some() {
            if watch.pending.is_empty() && !watch.overflow {
                watch.retry = Some(WatchPending {
                    reason: WatchReason::Retry,
                    assignment: update.desired.clone(),
                    bytes: 0,
                });
            }
        } else {
            watch.evaluated = Some(update.desired.assignment.clone());
            watch.retry = None;
        }
        let upserts = update.changes.iter().filter_map(|change| match change {
            AssignmentChange::Upsert { file, .. } => Some(file),
            AssignmentChange::Remove(_) => None,
        });
        Ok(upserts
            .zip(decisions)
            .filter(|(file, decision)| {
                let state = decision
                    .diagnostic(failure)
                    .map_or(ConfigApplyState::Acknowledged, |text| {
                        ConfigApplyState::Error(text.to_owned())
                    });
                self.complete_current(file, state)
            })
            .count())
    }

    pub async fn poll(&mut self) -> SessionPoll {
        let result = match self.fetcher.fetch_once().await {
            Ok(Some(revisions)) => self.accept(revisions),
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        };
        if let Ok(batch) = &result {
            for watch in self.watches.entries.values_mut() {
                watch.observe(batch.as_ref());
            }
        }
        SessionPoll {
            result,
            next_poll_after: self.fetcher.get_refresh_interval(),
        }
    }

    /// Completes the exact assignment that produced `file`. Returns false for stale receipts.
    pub fn complete(&self, file: &AssignedFile, state: ConfigApplyState) -> bool {
        self.current_file(file) && self.complete_raw(file, state)
    }

    /// Completes a raw revision observed at `generation`, without retaining its payload.
    /// Call this on the session that delivered the observation.
    pub fn complete_observed(&self, path: &str, generation: u64, state: ConfigApplyState) -> bool {
        let Some(desired) = self
            .desired
            .as_ref()
            .filter(|desired| generation <= desired.generation)
        else {
            return false;
        };
        let Ok(path) = RemoteConfigPath::try_parse(path) else {
            return false;
        };
        desired.positions.get(&path).is_some_and(|&index| {
            let file = &desired.files[index];
            file.assigned_at <= generation && self.complete_raw(file, state)
        })
    }

    /// Reports whether a nonempty batch for this assignment is covered by live watches.
    pub fn batch_fully_watched(&self, batch: &AssignmentBatch) -> bool {
        !self.watches.entries.is_empty()
            && !batch.changes.is_empty()
            && self
                .snapshot()
                .is_some_and(|desired| Arc::ptr_eq(desired, &batch.desired))
            && batch.changes.iter().all(|change| {
                self.watches.entries.values().any(|watch| {
                    watch.ready && !watch.closing && !watch.overflow && watch.owns(change.file())
                })
            })
    }

    fn complete_current(&self, file: &AssignedFile, state: ConfigApplyState) -> bool {
        self.current_file(file) && self.complete_file(file, state)
    }

    fn current_file(&self, file: &AssignedFile) -> bool {
        self.desired
            .as_ref()
            .and_then(|desired| desired.get(file.path()))
            .is_some_and(|current| std::ptr::eq(current.as_ref(), file))
    }

    fn complete_raw(&self, file: &AssignedFile, state: ConfigApplyState) -> bool {
        !self.watches.entries.values().any(|watch| watch.owns(file))
            && self.complete_file(file, state)
    }

    fn complete_file(&self, file: &AssignedFile, state: ConfigApplyState) -> bool {
        self.fetcher
            .fetcher
            .complete_file(file.path(), &file.revision, state)
    }

    fn accept(&mut self, revisions: Vec<Arc<Revision>>) -> anyhow::Result<Option<AssignmentBatch>> {
        let previous = self.desired.as_ref();
        if previous.is_some_and(|desired| {
            desired.files.len() == revisions.len()
                && revisions.iter().all(|revision| {
                    desired
                        .get(&revision.path)
                        .is_some_and(|file| Arc::ptr_eq(&file.revision, revision))
                })
        }) {
            return Ok(None);
        }

        let generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("assignment generation exhausted"))?;
        let mut changes = Vec::new();
        let previous_len = previous.map_or(0, |desired| desired.files.len());
        let mut ordered = Vec::with_capacity(previous_len.max(revisions.len()));
        ordered.resize_with(previous_len, || None);
        for (response_position, revision) in revisions.into_iter().enumerate() {
            let position =
                previous.and_then(|desired| desired.positions.get(revision.path.as_ref()).copied());
            let old = previous
                .zip(position)
                .map(|(desired, index)| &desired.files[index]);
            let file = match old {
                Some(file) if Arc::ptr_eq(&file.revision, &revision) => file.clone(),
                _ => {
                    let hashes = self
                        .fetcher
                        .fetcher
                        .file_hashes(&revision.path)
                        .ok_or_else(|| {
                            anyhow::anyhow!("missing verified metadata for {}", revision.path)
                        })?;
                    let file = Arc::new(AssignedFile {
                        revision,
                        hashes,
                        assigned_at: generation,
                        response_position,
                    });
                    changes.push(AssignmentChange::Upsert {
                        file: file.clone(),
                        previous: old.cloned(),
                    });
                    file
                }
            };
            if let Some(position) = position {
                ordered[position] = Some(file);
            } else {
                ordered.push(Some(file));
            }
        }
        if let Some(previous) = previous {
            for (file, replacement) in previous.files.iter().zip(&ordered) {
                if replacement.is_none() {
                    changes.push(AssignmentChange::Remove(file.clone()));
                }
            }
        }
        let files = ordered.into_iter().flatten().collect();
        let desired = Arc::new(Assignment::new(generation, self.target_generation, files));
        self.generation = generation;
        self.desired = Some(desired.clone());
        Ok(Some(AssignmentBatch { changes, desired }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::test_server::RemoteConfigServer;
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_networking::http_common;

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn retains_agent_assignment_on_no_content_or_not_found() {
        let server = RemoteConfigServer::spawn();
        let path = RemoteConfigPath::parse("employee/APM_TRACING/test/config").unwrap();
        let target = Target::new("service".into(), "env".into(), "1.0".into(), vec![], vec![]);
        let targets = vec![Arc::new(target.clone())];
        let mut session = Session::open(
            target,
            "runtime".into(),
            "client".into(),
            server.dummy_options(),
            NativeCapabilities::new_periodic(),
        )
        .await
        .unwrap();
        let no_change = [http::StatusCode::NO_CONTENT, http::StatusCode::NOT_FOUND];

        for status in no_change {
            let response = http_common::empty_response(http::Response::builder().status(status));
            *server.next_response.lock().unwrap() = Some(response.unwrap());
            assert!(session.poll().await.result.unwrap().is_none());
            assert!(session.snapshot().is_none());
        }

        server
            .files
            .lock()
            .unwrap()
            .insert(path, (targets, 1, "first".into()));
        let assigned = session.poll().await.result.unwrap().unwrap().desired;
        assert!(session.complete(
            assigned.files().next().unwrap(),
            ConfigApplyState::Acknowledged
        ));

        for status in no_change {
            let response = http_common::empty_response(http::Response::builder().status(status));
            *server.next_response.lock().unwrap() = Some(response.unwrap());
            assert!(session.poll().await.result.unwrap().is_none());
            assert!(Arc::ptr_eq(session.snapshot().unwrap(), &assigned));
            assert_eq!(reported_state(&server), (1, 2));
        }

        assert!(session.poll().await.result.unwrap().is_none());
        assert_eq!(reported_state(&server), (1, 2));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn polls_versioned_assignments_and_reports_only_current_completions() {
        let server = RemoteConfigServer::spawn();
        let path = RemoteConfigPath::parse("employee/APM_TRACING/test/config").unwrap();
        let target = Target::new("service".into(), "env".into(), "1.0".into(), vec![], vec![]);
        let targets = vec![Arc::new(target.clone())];
        let mut session = Session::open(
            target,
            "runtime".into(),
            "client".into(),
            server.dummy_options(),
            NativeCapabilities::new_periodic(),
        )
        .await
        .unwrap();

        assert!(session.snapshot().is_none());
        assert!(session.poll().await.result.unwrap().is_none());
        assert!(session.snapshot().is_none());

        server
            .files
            .lock()
            .unwrap()
            .insert(path.clone(), (targets.clone(), 1, "first".into()));
        let original = session.poll().await.result.unwrap().unwrap();
        let [
            AssignmentChange::Upsert {
                file: original_file,
                previous: None,
            },
        ] = original.changes.as_slice()
        else {
            panic!("expected a new assignment");
        };
        assert_eq!(original_file.path(), &path);
        assert_eq!(original_file.version(), 1);
        assert_eq!(original_file.hashes()[0].algorithm, "sha256");

        assert!(session.poll().await.result.unwrap().is_none());
        assert_eq!(reported_state(&server), (1, 1));
        assert!(Arc::ptr_eq(session.snapshot().unwrap(), &original.desired));

        server
            .files
            .lock()
            .unwrap()
            .insert(path.clone(), (targets, 2, "second".into()));
        let replacement = session.poll().await.result.unwrap().unwrap();
        let [
            AssignmentChange::Upsert {
                file: replacement_file,
                previous: Some(previous),
            },
        ] = replacement.changes.as_slice()
        else {
            panic!("expected a replacement assignment");
        };
        assert!(Arc::ptr_eq(original_file, previous));
        assert_eq!(original_file.bytes(), b"first");
        assert_eq!(replacement_file.bytes(), b"second");
        assert!(!session.complete(original_file, ConfigApplyState::Acknowledged));
        assert!(session.poll().await.result.unwrap().is_none());
        assert_eq!(reported_state(&server), (2, 1));

        assert!(session.complete(replacement_file, ConfigApplyState::Acknowledged));
        server.files.lock().unwrap().remove(&path);
        let removal = session.poll().await.result.unwrap().unwrap();
        assert_eq!(reported_state(&server), (2, 2));
        assert_eq!(removal.desired.files().len(), 0);
        let [AssignmentChange::Remove(removed)] = removal.changes.as_slice() else {
            panic!("expected a removal");
        };
        assert!(Arc::ptr_eq(replacement_file, removed));
        assert!(!session.complete(replacement_file, ConfigApplyState::Acknowledged));
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn retarget_requires_an_explicit_assignment_and_new_receipts() {
        let server = RemoteConfigServer::spawn();
        let path = RemoteConfigPath::parse("employee/APM_TRACING/test/config").unwrap();
        let original_target =
            Target::new("original".into(), "env".into(), "1".into(), vec![], vec![]);
        let new_target = Target::new("new".into(), "env".into(), "1".into(), vec![], vec![]);
        server.files.lock().unwrap().insert(
            path.clone(),
            (
                vec![
                    Arc::new(original_target.clone()),
                    Arc::new(new_target.clone()),
                ],
                1,
                "cached".into(),
            ),
        );
        let mut session = Session::open(
            original_target,
            "runtime".into(),
            "client".into(),
            server.dummy_options(),
            NativeCapabilities::new_periodic(),
        )
        .await
        .unwrap();
        let original = session.poll().await.result.unwrap().unwrap().desired;
        let old_file = original.files().next().unwrap();
        assert!(session.complete(old_file, ConfigApplyState::Acknowledged));

        session.set_identity("client".into(), "new-runtime".into(), vec![], vec![], None);
        assert!(session.poll().await.result.unwrap().is_none());
        assert!(Arc::ptr_eq(session.snapshot().unwrap(), &original));
        assert_eq!(reported_state(&server), (1, 2));

        let new_generation = session.retarget(new_target).unwrap();
        assert!(new_generation > original.target_generation);
        assert!(session.snapshot().is_none());
        assert!(!session.complete(old_file, ConfigApplyState::Acknowledged));

        *server.next_response.lock().unwrap() =
            Some(http::Response::new(http_common::Body::from("{}")));
        assert!(session.poll().await.result.unwrap().is_none());
        assert!(session.snapshot().is_none());
        {
            let request = server.last_request.lock().unwrap();
            let request = request.as_ref().unwrap();
            let client = request.client.as_ref().unwrap();
            let tracer = client.client_tracer.as_ref().unwrap();
            let state = client.state.as_ref().unwrap();
            assert_eq!(tracer.service, "new");
            assert_eq!(tracer.runtime_id, "new-runtime");
            assert_eq!(state.targets_version, 0);
            assert!(state.backend_client_state.is_empty());
            assert!(state.config_states.is_empty());
            assert_eq!(request.cached_target_files[0].path, path.to_string());
        }

        let batch = session.poll().await.result.unwrap().unwrap();
        assert_eq!(batch.desired.target_generation, new_generation);
        assert!(batch.desired.generation > original.generation);
        let [
            AssignmentChange::Upsert {
                file,
                previous: None,
            },
        ] = batch.changes.as_slice()
        else {
            panic!("expected the cached file to be independently assigned to the new target");
        };
        assert_eq!(file.bytes(), b"cached");
        assert_eq!(old_file.bytes(), file.bytes());
        assert!(!session.complete(old_file, ConfigApplyState::Acknowledged));
        assert!(session.poll().await.result.unwrap().is_none());
        assert_eq!(reported_state(&server), (1, 1));
        assert!(session.complete(file, ConfigApplyState::Acknowledged));
        assert!(session.poll().await.result.unwrap().is_none());
        assert_eq!(reported_state(&server), (1, 2));
    }

    async fn watched_session() -> (
        Arc<RemoteConfigServer>,
        Session<NativeCapabilities>,
        Arc<Target>,
    ) {
        let server = RemoteConfigServer::spawn();
        let target = Target::new("service".into(), "env".into(), "1.0".into(), vec![], vec![]);
        let session = Session::open(
            target.clone(),
            "runtime".into(),
            "client".into(),
            server.dummy_options(),
            NativeCapabilities::new_periodic(),
        )
        .await
        .unwrap();
        (server, session, Arc::new(target))
    }

    fn assign(server: &RemoteConfigServer, target: &Arc<Target>, version: u64) {
        server.files.lock().unwrap().insert(
            RemoteConfigPath::parse("employee/APM_TRACING/test/config").unwrap(),
            (vec![target.clone()], version, format!("value-{version}")),
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn latest_watch_coalesces_and_retries_with_current_receipts() {
        let (server, mut session, target) = watched_session().await;
        let product = RemoteConfigProduct::ApmTracing;
        let watch = session
            .open_watch(vec![product], WatchMode::LatestState)
            .unwrap();
        assert!(
            session
                .open_watch(vec![product], WatchMode::LatestState)
                .is_err()
        );
        assert!(!session.replay_watch(watch).unwrap());
        assert!(session.poll().await.result.unwrap().is_none());
        assert!(
            session
                .next_watch_with_reservation(watch, || Ok(()))
                .unwrap()
                .is_none()
        );

        assign(&server, &target, 1);
        session.poll().await.result.unwrap().unwrap();
        let initial = session
            .next_watch_with_reservation(watch, || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(initial.reason, WatchReason::Initial);
        assert_eq!(initial.desired.files().next().unwrap().version(), 1);
        assert!(!session.complete(
            initial.desired.files().next().unwrap(),
            ConfigApplyState::Acknowledged
        ));

        for version in [2, 3] {
            assign(&server, &target, version);
            session.poll().await.result.unwrap().unwrap();
            assert!(
                session
                    .next_watch_with_reservation(watch, || Ok(()))
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            session
                .finish_watch(
                    watch,
                    initial.ticket,
                    WatchOutcome::Applied,
                    &[WatchDecision::Eligible]
                )
                .unwrap(),
            0
        );
        let changed = session
            .next_watch_with_reservation(watch, || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(changed.reason, WatchReason::Changed);
        let [
            AssignmentChange::Upsert {
                file,
                previous: Some(previous),
            },
        ] = changed.changes()
        else {
            panic!("expected one coalesced upsert");
        };
        assert_eq!((previous.version(), file.version()), (1, 3));
        assert!(
            session
                .finish_watch(watch, changed.ticket, WatchOutcome::Applied, &[])
                .is_err()
        );
        assert_eq!(
            session
                .finish_watch(
                    watch,
                    changed.ticket,
                    WatchOutcome::Failed("installer busy"),
                    &[WatchDecision::Eligible]
                )
                .unwrap(),
            1
        );
        assert!(
            session
                .next_watch_with_reservation(watch, || Ok(()))
                .unwrap()
                .is_none()
        );

        assert!(session.poll().await.result.unwrap().is_none());
        assert_eq!(reported_state(&server), (3, 3));
        let retry = session
            .next_watch_with_reservation(watch, || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(retry.reason, WatchReason::Retry);
        assert_ne!(retry.ticket, changed.ticket);
        assert!(
            session
                .finish_watch(
                    watch,
                    changed.ticket,
                    WatchOutcome::Applied,
                    &[WatchDecision::Eligible]
                )
                .is_err()
        );
        assert_eq!(
            session
                .finish_watch(
                    watch,
                    retry.ticket,
                    WatchOutcome::Applied,
                    &[WatchDecision::Eligible]
                )
                .unwrap(),
            1
        );
        assert!(session.poll().await.result.unwrap().is_none());
        assert_eq!(reported_state(&server), (3, 2));

        assert!(session.replay_watch(watch).unwrap());
        let replay = session
            .next_watch_with_reservation(watch, || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(replay.reason, WatchReason::Replay);
        assert!(replay.changes().is_empty());
        assert!(!session.close_watch(watch));
        assert!(
            session
                .open_watch(vec![product], WatchMode::LatestState)
                .is_err()
        );
        assert_eq!(
            session
                .finish_watch(watch, replay.ticket, WatchOutcome::Retained, &[])
                .unwrap(),
            0
        );
        let late = session
            .open_watch(vec![product], WatchMode::LatestState)
            .unwrap();
        assert_eq!(
            session
                .next_watch_with_reservation(late, || Ok(()))
                .unwrap()
                .unwrap()
                .reason,
            WatchReason::Replay
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn ordered_watch_reports_overflow_and_retarget_drains_old_install() {
        let (server, mut session, target) = watched_session().await;
        let mode = WatchMode::OrderedState { max_pending: 2 };
        let watch = session
            .open_watch(vec![RemoteConfigProduct::ApmTracing], mode)
            .unwrap();
        assign(&server, &target, 1);
        session.poll().await.result.unwrap().unwrap();
        let first = session
            .next_watch_with_reservation(watch, || Ok(()))
            .unwrap()
            .unwrap();
        for version in [2, 3] {
            assign(&server, &target, version);
            session.poll().await.result.unwrap().unwrap();
        }
        assert!(
            session
                .next_watch_with_reservation(watch, || Ok(()))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            session
                .finish_watch(
                    watch,
                    first.ticket,
                    WatchOutcome::Applied,
                    &[WatchDecision::Ignored]
                )
                .unwrap(),
            0
        );
        let second = session
            .next_watch_with_reservation(watch, || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(second.desired.files().next().unwrap().version(), 2);
        for version in [4, 5] {
            assign(&server, &target, version);
            session.poll().await.result.unwrap().unwrap();
        }
        let overflow = session
            .next_watch_with_reservation(watch, || Ok(()))
            .err()
            .unwrap();
        assert!(overflow.is::<WatchOverflow>());
        assert!(session.replay_watch(watch).is_err());

        let generation = session.retarget(target.as_ref().clone()).unwrap();
        assert!(session.snapshot().is_none());
        assign(&server, &target, 6);
        session.poll().await.result.unwrap().unwrap();
        assert!(
            session
                .next_watch_with_reservation(watch, || Ok(()))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            session
                .finish_watch(watch, second.ticket, WatchOutcome::Applied, &[])
                .unwrap(),
            0
        );
        let reset = session
            .next_watch_with_reservation(watch, || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(reset.reason, WatchReason::Initial);
        assert_eq!(reset.target_generation(), generation);
        assert_eq!(reset.desired.files().next().unwrap().version(), 6);
        assert_eq!(
            session
                .finish_watch(
                    watch,
                    reset.ticket,
                    WatchOutcome::Retained,
                    &[WatchDecision::Rejected("schema error")]
                )
                .unwrap(),
            1
        );
        assert!(session.poll().await.result.unwrap().is_none());
        assert_eq!(reported_state(&server), (6, 3));
    }

    #[test]
    fn ordered_watch_counts_only_its_own_queued_payloads() {
        let mut registry = WatchRegistry::default();
        let id = registry
            .prepare_watch(
                vec![RemoteConfigProduct::ApmTracing],
                WatchMode::OrderedState { max_pending: 2 },
            )
            .unwrap();
        let watch = registry.entries.get_mut(&id).unwrap();
        let assignment_file = |path, version, bytes| {
            Arc::new(AssignedFile {
                revision: VersionedStorage.store(version, path, bytes).unwrap(),
                hashes: Vec::new(),
                assigned_at: version,
                response_position: 0,
            })
        };
        let sibling = assignment_file(
            Arc::new(RemoteConfigPath::parse("employee/LIVE_DEBUGGING/sibling/config").unwrap()),
            1,
            vec![0; MAX_WATCH_QUEUED_BYTES / 2],
        );
        let unrelated = Arc::downgrade(&sibling);
        let path = Arc::new(RemoteConfigPath::parse("employee/APM_TRACING/test/config").unwrap());
        let mut previous = None;
        for version in [1, 2] {
            let file = assignment_file(path.clone(), version, vec![b'1']);
            watch.observe(Some(&AssignmentBatch {
                desired: Arc::new(Assignment::new(
                    version,
                    1,
                    vec![sibling.clone(), file.clone()],
                )),
                changes: vec![AssignmentChange::Upsert {
                    file: file.clone(),
                    previous: previous.replace(file),
                }],
            }));
        }
        drop(sibling);
        assert!(unrelated.upgrade().is_none());

        let first = watch.next(|| Ok(())).unwrap().unwrap();
        let files: Vec<_> = first.desired.files().collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].version(), 1);
        assert_eq!(files[0].path(), path.as_ref());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn watch_scopes_the_ready_assignment_and_removals() {
        let (server, mut session, target) = watched_session().await;
        let watch = session
            .open_watch(
                vec![RemoteConfigProduct::ApmTracing],
                WatchMode::LatestState,
            )
            .unwrap();
        let sibling = RemoteConfigPath::parse("employee/LIVE_DEBUGGING/sibling/config").unwrap();
        server
            .files
            .lock()
            .unwrap()
            .insert(sibling.clone(), (vec![target.clone()], 1, "sibling".into()));
        let batch = session.poll().await.result.unwrap().unwrap();
        let first = session
            .next_watch_with_reservation(watch, || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(first.reason, WatchReason::Initial);
        assert!(first.desired.files().next().is_none());
        assert!(first.changes().is_empty());
        assert!(session.complete(
            batch.desired.files().next().unwrap(),
            ConfigApplyState::Acknowledged
        ));
        assert_eq!(
            session
                .finish_watch(watch, first.ticket, WatchOutcome::Applied, &[])
                .unwrap(),
            0
        );

        server.files.lock().unwrap().insert(
            sibling.clone(),
            (vec![target.clone()], 2, "updated sibling".into()),
        );
        session.poll().await.result.unwrap().unwrap();
        assert!(
            session
                .next_watch_with_reservation(watch, || Ok(()))
                .unwrap()
                .is_none()
        );

        assign(&server, &target, 1);
        session.poll().await.result.unwrap().unwrap();
        let added = session
            .next_watch_with_reservation(watch, || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(added.desired.files().count(), 1);
        assert_eq!(
            session
                .finish_watch(
                    watch,
                    added.ticket,
                    WatchOutcome::Retained,
                    &[WatchDecision::Ignored]
                )
                .unwrap(),
            1
        );

        server
            .files
            .lock()
            .unwrap()
            .retain(|path, _| path == &sibling);
        session.poll().await.result.unwrap().unwrap();
        let emptied = session
            .next_watch_with_reservation(watch, || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(emptied.reason, WatchReason::Changed);
        assert!(matches!(emptied.changes(), [AssignmentChange::Remove(_)]));
        assert!(emptied.desired.files().next().is_none());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn ordered_watch_drops_a_retry_superseded_before_dispatch() {
        let (server, mut session, target) = watched_session().await;
        let mode = WatchMode::OrderedState { max_pending: 1 };
        let watch = session
            .open_watch(vec![RemoteConfigProduct::ApmTracing], mode)
            .unwrap();
        assign(&server, &target, 1);
        session.poll().await.result.unwrap().unwrap();
        let first = session
            .next_watch_with_reservation(watch, || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(
            session
                .finish_watch(
                    watch,
                    first.ticket,
                    WatchOutcome::Failed("installer busy"),
                    &[WatchDecision::Eligible]
                )
                .unwrap(),
            1
        );
        assert!(session.poll().await.result.unwrap().is_none());

        assign(&server, &target, 2);
        session.poll().await.result.unwrap().unwrap();
        let next = session
            .next_watch_with_reservation(watch, || Ok(()))
            .unwrap()
            .unwrap();
        assert_eq!(next.reason, WatchReason::Changed);
        assert_eq!(next.desired.files().next().unwrap().version(), 2);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn ordered_watch_reapplies_a_failed_revision_when_a_sibling_changes() {
        for queue_before_failure in [false, true] {
            let (server, mut session, target) = watched_session().await;
            let watch = session
                .open_watch(
                    vec![RemoteConfigProduct::ApmTracing],
                    WatchMode::OrderedState { max_pending: 1 },
                )
                .unwrap();
            assign(&server, &target, 1);
            session.poll().await.result.unwrap().unwrap();
            let initial = session
                .next_watch_with_reservation(watch, || Ok(()))
                .unwrap()
                .unwrap();
            let eligible = WatchDecision::Eligible;
            assert_eq!(
                session
                    .finish_watch(watch, initial.ticket, WatchOutcome::Applied, &[eligible])
                    .unwrap(),
                1
            );

            assign(&server, &target, 2);
            session.poll().await.result.unwrap().unwrap();
            let failed = session
                .next_watch_with_reservation(watch, || Ok(()))
                .unwrap()
                .unwrap();
            let sibling = RemoteConfigPath::parse("employee/APM_TRACING/sibling/config").unwrap();
            let assign_sibling = || {
                server
                    .files
                    .lock()
                    .unwrap()
                    .insert(sibling.clone(), (vec![target.clone()], 1, "sibling".into()));
            };
            if queue_before_failure {
                assign_sibling();
                session.poll().await.result.unwrap().unwrap();
            }
            assert_eq!(
                session
                    .finish_watch(
                        watch,
                        failed.ticket,
                        WatchOutcome::Failed("installer busy"),
                        &[WatchDecision::Eligible],
                    )
                    .unwrap(),
                1
            );
            if !queue_before_failure {
                assert!(session.poll().await.result.unwrap().is_none());
                assign_sibling();
                session.poll().await.result.unwrap().unwrap();
            }

            let next = session
                .next_watch_with_reservation(watch, || Ok(()))
                .unwrap()
                .unwrap();
            assert_eq!(next.reason, WatchReason::Changed);
            assert_eq!(
                next.changes().len(),
                2,
                "queued early: {queue_before_failure}"
            );
            assert!(next.changes().iter().any(|change| matches!(change,
                AssignmentChange::Upsert { file, previous: Some(previous) }
                    if file.version() == 2 && previous.version() == 1)));
            assert_eq!(
                session
                    .finish_watch(
                        watch,
                        next.ticket,
                        WatchOutcome::Applied,
                        &[WatchDecision::Eligible, WatchDecision::Eligible],
                    )
                    .unwrap(),
                2
            );
            assert!(session.poll().await.result.unwrap().is_none());
            let request = server.last_request.lock().unwrap();
            let states = &request
                .as_ref()
                .unwrap()
                .client
                .as_ref()
                .unwrap()
                .state
                .as_ref()
                .unwrap()
                .config_states;
            assert_eq!(states.len(), 2);
            assert!(states.iter().all(|state| state.apply_state == 2));
        }
    }

    fn reported_state(server: &RemoteConfigServer) -> (u64, u64) {
        let request = server.last_request.lock().unwrap();
        let state = request
            .as_ref()
            .unwrap()
            .client
            .as_ref()
            .unwrap()
            .state
            .as_ref()
            .unwrap();
        let [config_state] = state.config_states.as_slice() else {
            panic!("expected a single apply state");
        };
        (config_state.version, config_state.apply_state)
    }
}
