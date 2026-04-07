// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    fmt::{self, Debug},
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex, MutexGuard,
    },
    time::{Duration, Instant},
};

use libdd_shared_runtime::Worker;

use crate::trace_exporter::{
    agent_response::AgentResponse, error::TraceExporterError, TraceExporter,
};

#[derive(Clone, Copy, Debug)]
pub struct TraceBufferConfig {
    synchronous_export: bool,
    synchronous_export_timeout: Option<Duration>,
    max_flush_interval: Duration,
    max_buffered_spans: usize,
    span_flush_threshold: usize,
}

impl TraceBufferConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the async exporter waits for the trace chunks to be exported before returning from
    /// export_chunk
    pub fn synchronous_export(self, synchronous_writes: bool) -> Self {
        Self {
            synchronous_export: synchronous_writes,
            ..self
        }
    }

    /// The maximum amount of time the export_chunk waits for a flush if synchronous_writes is
    /// enabled. If this is zero send_chunk will always return an error
    ///
    /// If it is None, the send will wait forever
    pub fn synchronous_export_timeout(self, timeout: Option<Duration>) -> Self {
        Self {
            synchronous_export_timeout: timeout,
            ..self
        }
    }

    /// The maximum amount of time between two flushes
    pub fn max_flush_interval(self, interval: Duration) -> Self {
        Self {
            max_flush_interval: interval,
            ..self
        }
    }

    /// The maximum number of spans that will be buffered before we drop data
    pub fn max_buffered_spans(self, max: usize) -> Self {
        Self {
            max_buffered_spans: max,
            ..self
        }
    }

    /// The number of spans that will be buffered before we decide to flush
    pub fn span_flush_threshold(self, threshold: usize) -> Self {
        Self {
            span_flush_threshold: threshold,
            ..self
        }
    }
}

impl Default for TraceBufferConfig {
    fn default() -> Self {
        Self {
            synchronous_export: false,
            synchronous_export_timeout: Some(Duration::from_secs(1)),
            max_flush_interval: Duration::from_secs(2),
            max_buffered_spans: 10_000,
            span_flush_threshold: 3_000,
        }
    }
}

pub type TraceChunk<T> = Vec<T>;

/// Error that can occur when the batch has reached its maximum size
/// and we can't add more spans to it.
///
/// The added spans will be dropped.
#[derive(Debug, PartialEq, Eq)]
pub struct BatchFullError {
    spans_dropped: usize,
}

/// Error that can occur when the mutex was poisoned.
///
/// The only way to handle it is to log and try to return an empty but valid state
#[derive(Debug)]
struct MutexPoisonedError;

#[derive(Debug)]
pub enum TraceBufferError {
    AlreadyShutdown,
    TimedOut(std::time::Duration),
    MutexPoisoned,
    BatchFull(BatchFullError),
    TraceExporter(TraceExporterError),
}

// AtomicU64 bit layout:
//   bit 63: HAS_SHUTDOWN_BIT
//   bit 62: FLUSH_NEEDED_BIT
//   bits 0-61: span_count
const HAS_SHUTDOWN_BIT: u64 = 1 << 63;
const FLUSH_NEEDED_BIT: u64 = 1 << 62;
const SPAN_COUNT_MASK: u64 = (1 << 62) - 1;

struct SyncState {
    /// Current batch generation - Used in synchronous mode
    /// Senders in synchronous mode capture this under the lock so their chunk is guaranteed
    /// to be part of exactly this generation's drain cycle.
    /// The receiver increment it after draining the queue
    batch_gen: BatchGeneration,
    /// Latest fully exported generation — incremented by worker after ack_export.
    /// Synchronous senders wait until this reaches their captured batch_gen.
    last_flush_generation: BatchGeneration,
    /// Set by the worker on shutdown. Used by wait_shutdown_done condvar wait.
    has_shutdown: bool,
}

impl SyncState {
    fn new() -> Self {
        Self {
            batch_gen: BatchGeneration(1),
            last_flush_generation: BatchGeneration(0),
            has_shutdown: false,
        }
    }
}

#[derive(Default)]
struct AtomicQueueMetrics {
    spans_queued: AtomicU64,
    spans_dropped_full_buffer: AtomicU64,
}

struct Shared {
    /// Packed atomic: [HAS_SHUTDOWN(1b) | FLUSH_NEEDED(1b) | span_count(62b)]
    atomic_state: AtomicU64,
    receiver_notifier: tokio::sync::Notify,
    sync_state: Mutex<SyncState>,
    sender_notifier: Condvar,
    metrics: AtomicQueueMetrics,
}

impl Shared {
    fn sync_state(&self) -> Result<MutexGuard<'_, SyncState>, MutexPoisonedError> {
        self.sync_state.lock().map_err(|_| MutexPoisonedError)
    }

    fn notify_receiver(&self) {
        self.receiver_notifier.notify_one();
    }

    fn notify_sender(&self, state: MutexGuard<'_, SyncState>) {
        drop(state);
        self.sender_notifier.notify_all();
    }
}

/// # TraceBuffer
///
/// Creating an instance of the TraceBuffer will spawn a background thread that
/// periodically sends trace chunks through the TraceExporter
///
/// # Buffering behavior
///
/// Unless in synchronous mode, when [`TraceBuffer::send_chunk`] is called, the trace chunk
/// will be buffered until:
/// * The number of spans in the buffer is greater than [`TraceBufferConfig::span_flush_threshold`]
/// * The time since the last flush is greater than [`TraceBufferConfig::max_flush_interval`]
/// * [`TraceBuffer::force_flush`] is called. This method triggers a flush, but do not wait for the
///   flush to be done before returning
///
/// # Synchronous mode
///
/// If [`TraceBufferConfig::synchronous_writes`] is true and
/// * Either until the chunks have been flushed the agent
/// * Or if `synchronous_writes_timeout` is Some, until the timeout is reached. At which point the
///   flush might continue in the background
pub struct TraceBuffer<T> {
    tx: Sender<T>,
    /// Enables synchronous exports if Some
    ///
    /// Each batch in the queue will get a generation associated. Generations are strictly
    /// incremental and processed in order by the background thread.
    /// When the background thread processes a batch it will increment it's 'last_flushed_batch'
    /// and an export can wait until the 'last_flushed_batch' is equal to the batch it added it's
    /// trace chunks to.
    synchronous_export: bool,
    synchronous_export_timeout: Option<Duration>,
}

pub type ResponseHandler = Box<dyn Fn(Result<AgentResponse, TraceExporterError>) + Send + Sync>;

impl<T: Send + 'static> TraceBuffer<T> {
    pub fn new(
        config: TraceBufferConfig,
        response_handler: ResponseHandler,
        export_operation: Box<dyn Export<T> + Send + Sync>,
        trace_exporter: TraceExporter,
    ) -> (Self, TraceExporterWorker<T>) {
        let (tx, rx) = channel(
            config.span_flush_threshold,
            config.max_buffered_spans,
            config.synchronous_export,
        );
        let worker = {
            TraceExporterWorker::new(
                trace_exporter,
                rx,
                response_handler,
                export_operation,
                config,
            )
        };
        (
            Self {
                tx,
                synchronous_export: config.synchronous_export,
                synchronous_export_timeout: config.synchronous_export_timeout,
            },
            worker,
        )
    }

    pub fn send_chunk(&self, trace_chunk: Vec<T>) -> Result<(), TraceBufferError> {
        let chunk_len = trace_chunk.len();
        if chunk_len == 0 {
            return Ok(());
        }

        match self.tx.add_trace_chunk(trace_chunk) {
            Ok(flush_gen) => {
                if self.synchronous_export {
                    self.tx
                        .wait_flush_done(flush_gen, self.synchronous_export_timeout)?;
                }
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub fn force_flush(&self) -> Result<(), TraceBufferError> {
        self.tx.trigger_flush()
    }

    pub fn queue_metrics(&self) -> QueueMetricsFetcher {
        QueueMetricsFetcher {
            shared: self.tx.shared.clone(),
        }
    }

    pub fn wait_shutdown_done(&self, timeout: Duration) -> Result<(), TraceBufferError> {
        self.tx.wait_shutdown_done(timeout)
    }
}

impl<T> fmt::Debug for TraceBuffer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TraceBuffer").finish()
    }
}

pub struct QueueMetricsFetcher {
    shared: Arc<Shared>,
}

impl QueueMetricsFetcher {
    pub fn get_metrics(&self) -> QueueMetrics {
        QueueMetrics {
            spans_queued: self.shared.metrics.spans_queued.swap(0, Ordering::Relaxed) as usize,
            spans_dropped_full_buffer: self
                .shared
                .metrics
                .spans_dropped_full_buffer
                .swap(0, Ordering::Relaxed) as usize,
        }
    }
}

#[derive(Default)]
pub struct QueueMetrics {
    pub spans_dropped_full_buffer: usize,
    pub spans_queued: usize,
}

fn channel<T>(
    flush_trigger_number_of_spans: usize,
    max_number_of_spans: usize,
    synchronous_write: bool,
) -> (Sender<T>, Receiver<T>) {
    let (chunk_sender, chunk_receiver) = crossbeam_channel::unbounded();
    let waiter = Arc::new(Shared {
        atomic_state: AtomicU64::new(0),
        receiver_notifier: tokio::sync::Notify::new(),
        sync_state: Mutex::new(SyncState::new()),
        sender_notifier: Condvar::new(),
        metrics: AtomicQueueMetrics::default(),
    });
    (
        Sender {
            shared: waiter.clone(),
            chunk_sender,
            flush_trigger_number_of_spans,
            max_buffered_spans: max_number_of_spans,
            synchronous_write,
        },
        Receiver {
            chunk_receiver,
            waiter,
            last_flush: Instant::now(),
            synchronous_write,
        },
    )
}

struct Sender<T> {
    shared: Arc<Shared>,
    chunk_sender: crossbeam_channel::Sender<TraceChunk<T>>,
    flush_trigger_number_of_spans: usize,
    max_buffered_spans: usize,
    synchronous_write: bool,
}

impl<T> Sender<T> {
    fn wait_flush_done(
        &self,
        flush_gen: BatchGeneration,
        timeout: Option<Duration>,
    ) -> Result<(), TraceBufferError> {
        let cond =
            |state: &mut SyncState| state.last_flush_generation < flush_gen && !state.has_shutdown;

        let state = self
            .shared
            .sync_state()
            .map_err(|MutexPoisonedError| TraceBufferError::MutexPoisoned)?;

        if let Some(timeout) = timeout {
            if timeout.is_zero() {
                return Err(TraceBufferError::TimedOut(Duration::ZERO));
            }
            let (_state, res) = self
                .shared
                .sender_notifier
                .wait_timeout_while(state, timeout, cond)
                .map_err(|_| TraceBufferError::MutexPoisoned)?;
            if res.timed_out() {
                return Err(TraceBufferError::TimedOut(timeout));
            }
        } else {
            let _state = self
                .shared
                .sender_notifier
                .wait_while(state, cond)
                .map_err(|_| TraceBufferError::MutexPoisoned)?;
        }
        Ok(())
    }

    fn add_trace_chunk(&self, chunk: Vec<T>) -> Result<BatchGeneration, TraceBufferError> {
        let chunk_len = chunk.len();

        // Uses Acquire to pair with the worker's Release when setting HAS_SHUTDOWN_BIT.
        let state = self.shared.atomic_state.load(Ordering::Acquire);
        if state & HAS_SHUTDOWN_BIT != 0 {
            return Err(TraceBufferError::AlreadyShutdown);
        }
        if (state & SPAN_COUNT_MASK) as usize > self.max_buffered_spans {
            self.shared
                .metrics
                .spans_dropped_full_buffer
                .fetch_add(chunk_len as u64, Ordering::Relaxed);
            return Err(TraceBufferError::BatchFull(BatchFullError {
                spans_dropped: chunk_len,
            }));
        }

        let flush_gen = if self.synchronous_write {
            // In synchronous mode, hold the sync_state lock around the channel push so the
            // worker's drain (which also holds this lock) is guaranteed to see this chunk
            // as part of the generation it captured.
            let sync = self
                .shared
                .sync_state()
                .map_err(|MutexPoisonedError| TraceBufferError::MutexPoisoned)?;
            let gen = sync.batch_gen;
            if sync.has_shutdown {
                return Err(TraceBufferError::AlreadyShutdown);
            }
            let _ = self.chunk_sender.send(chunk);
            gen
        } else {
            let _ = self.chunk_sender.send(chunk);
            BatchGeneration::default()
        };

        self.shared
            .metrics
            .spans_queued
            .fetch_add(chunk_len as u64, Ordering::Relaxed);

        // AcqRel: Release orders the channel send before the span count update so the worker
        // sees a coherent view; Acquire sees latest flag state.
        let prev = self
            .shared
            .atomic_state
            .fetch_add(chunk_len as u64, Ordering::AcqRel);
        let new_span_count = (prev & SPAN_COUNT_MASK) + chunk_len as u64;

        if new_span_count > self.flush_trigger_number_of_spans as u64 || self.synchronous_write {
            // Release: orders all prior writes (channel send + span count) before the worker
            // observes FLUSH_NEEDED_BIT.
            let prev2 = self
                .shared
                .atomic_state
                .fetch_or(FLUSH_NEEDED_BIT, Ordering::Release);
            // Only the thread that transitions the bit 0→1 wakes the worker to avoid
            // thundering-herd notifications.
            if prev2 & FLUSH_NEEDED_BIT == 0 {
                self.shared.notify_receiver();
            }
        }

        Ok(flush_gen)
    }

    fn trigger_flush(&self) -> Result<(), TraceBufferError> {
        let state = self.shared.atomic_state.load(Ordering::Acquire);
        if state & HAS_SHUTDOWN_BIT != 0 {
            return Err(TraceBufferError::AlreadyShutdown);
        }
        let prev = self
            .shared
            .atomic_state
            .fetch_or(FLUSH_NEEDED_BIT, Ordering::Release);
        if prev & FLUSH_NEEDED_BIT == 0 {
            self.shared.notify_receiver();
        }
        Ok(())
    }

    fn wait_shutdown_done(&self, timeout: Duration) -> Result<(), TraceBufferError> {
        if timeout.is_zero() {
            return Err(TraceBufferError::TimedOut(Duration::ZERO));
        }
        let state = self
            .shared
            .sync_state()
            .map_err(|MutexPoisonedError| TraceBufferError::MutexPoisoned)?;
        let (_state, res) = self
            .shared
            .sender_notifier
            .wait_timeout_while(state, timeout, |s| !s.has_shutdown)
            .map_err(|_| TraceBufferError::MutexPoisoned)?;
        if res.timed_out() {
            return Err(TraceBufferError::TimedOut(timeout));
        }
        Ok(())
    }
}

struct Receiver<T> {
    waiter: Arc<Shared>,
    chunk_receiver: crossbeam_channel::Receiver<TraceChunk<T>>,
    last_flush: Instant,
    synchronous_write: bool,
}

impl<T> Receiver<T> {
    fn shutdown_done(&self) -> Result<(), MutexPoisonedError> {
        // Set the atomic bit first so senders' fast-path check sees it.
        self.waiter
            .atomic_state
            .fetch_or(HAS_SHUTDOWN_BIT, Ordering::Release);
        let mut state = self.waiter.sync_state()?;
        state.has_shutdown = true;
        self.waiter.notify_sender(state);
        Ok(())
    }

    fn reset(&mut self) -> Result<(), MutexPoisonedError> {
        // Drain all pending items from the channel
        // No sender should be running
        while self.chunk_receiver.try_recv().is_ok() {}

        // Reset atomic state: no shutdown, no flush needed, zero span count.
        // SeqCst: conservative ordering for the single-threaded post-fork context.
        self.waiter.atomic_state.store(0, Ordering::SeqCst);

        *self.waiter.sync_state()? = SyncState::new();
        self.last_flush = Instant::now();
        self.waiter.metrics.spans_queued.store(0, Ordering::Relaxed);
        self.waiter
            .metrics
            .spans_dropped_full_buffer
            .store(0, Ordering::Relaxed);
        Ok(())
    }

    fn drain_channel(&self) -> Vec<TraceChunk<T>> {
        // Clear FLUSH_NEEDED_BIT before draining so that a sender arriving during the drain can
        // set the bit again and schedule a fresh wakeup for its chunk.
        let to_consume = (self
            .waiter
            .atomic_state
            .fetch_and(!FLUSH_NEEDED_BIT, Ordering::AcqRel)
            & SPAN_COUNT_MASK) as usize;

        let mut chunks: Vec<TraceChunk<T>> = Vec::new();
        {
            // Hold the sync_state lock for the entire drain to serialize with senders'
            // channel pushes (which also hold this lock in synchronous mode).
            let _sync_guard = if self.synchronous_write {
                let mut guard = self.waiter.sync_state();
                if let Ok(sync) = &mut guard {
                    sync.batch_gen.incr();
                }
                Some(guard)
            } else {
                None
            };

            let mut consummed: usize = 0;
            while consummed < to_consume {
                match self.chunk_receiver.try_recv() {
                    Ok(chunk) => {
                        consummed += chunk.len();
                        chunks.push(chunk);
                    }
                    Err(_) => break,
                }
            }
            // Return the logical capacity taken by the consumed spans.
            // AcqRel: Release frees capacity visible to the next sender Acquire load.
            self.waiter
                .atomic_state
                .fetch_sub(consummed as u64, Ordering::AcqRel);
        }
        chunks
    }

    async fn receive(
        &mut self,
        timeout: Duration,
    ) -> Result<Vec<TraceChunk<T>>, MutexPoisonedError> {
        let batch = loop {
            // Enable the notify future BEFORE reading the atomic state to avoid lost wakeups:
            // any notify_one() that fires between enable() and .await is captured.
            let notified = self.waiter.receiver_notifier.notified();
            let mut notified = std::pin::pin!(notified);
            notified.as_mut().enable();

            let state = self.waiter.atomic_state.load(Ordering::Acquire);
            let flush_needed = state & FLUSH_NEEDED_BIT;
            if flush_needed != 0 {
                break self.drain_channel();
            }

            let deadline = self.last_flush + timeout;
            let leftover = deadline.saturating_duration_since(Instant::now());
            if leftover == Duration::ZERO {
                break self.drain_channel();
            }

            tokio::select! {
                biased;
                _ = notified.as_mut() => {}  // woken by sender; loop to re-check state
                _ = tokio::time::sleep(leftover) => {
                    break self.drain_channel();
                }
            }
        };
        self.last_flush = Instant::now();
        Ok(batch)
    }

    fn ack_export(&self) -> Result<(), MutexPoisonedError> {
        let mut state = self.waiter.sync_state()?;
        state.last_flush_generation.incr();
        self.waiter.notify_sender(state);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
struct BatchGeneration(u64);

impl BatchGeneration {
    fn incr(&mut self) {
        self.0 = self.0.wrapping_add(1);
    }
}

/// A pluggable export operation for the trace buffer
///
/// This allows mapping from the buffered spans to another type, and
/// calling any method on the trace exporter to send traces
pub trait Export<T>: Send + Debug {
    fn export_trace_chunks<'a>(
        &'a mut self,
        trace_chunks: Vec<TraceChunk<T>>,
        trace_exporter: &'a TraceExporter,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<AgentResponse, TraceExporterError>> + Send + 'a,
        >,
    >;
}

#[derive(Debug)]
pub struct DefaultExport;

impl Export<libdd_trace_utils::span::v04::SpanBytes> for DefaultExport {
    fn export_trace_chunks<'a>(
        &'a mut self,
        trace_chunks: Vec<TraceChunk<libdd_trace_utils::span::v04::SpanBytes>>,
        trace_exporter: &'a TraceExporter,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<AgentResponse, TraceExporterError>> + Send + 'a,
        >,
    > {
        Box::pin(async { trace_exporter.send_trace_chunks_async(trace_chunks).await })
    }
}

#[derive(Debug)]
struct TraceExporterRunInput<T> {
    trace_chunks: Vec<TraceChunk<T>>,
}

pub struct TraceExporterWorker<T> {
    trace_exporter: TraceExporter,
    rx: Receiver<T>,
    export_operation: Box<dyn Export<T> + Send + Sync>,
    agent_response_handler: ResponseHandler,
    config: TraceBufferConfig,

    run_input: Option<TraceExporterRunInput<T>>,
}

impl<T: Debug> std::fmt::Debug for TraceExporterWorker<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TraceExporterWorker")
            .field("trace_exporter", &self.trace_exporter)
            .field("export_operation", &self.export_operation)
            .field("config", &self.config)
            .field("run_input", &self.run_input)
            .finish()
    }
}

impl<T: Send + 'static> TraceExporterWorker<T> {
    fn new(
        trace_exporter: TraceExporter,
        rx: Receiver<T>,
        agent_response_handler: ResponseHandler,
        export_operation: Box<dyn Export<T> + Send + Sync>,
        config: TraceBufferConfig,
    ) -> Self {
        Self {
            trace_exporter,
            rx,
            agent_response_handler,
            export_operation,
            config,
            run_input: None,
        }
    }

    async fn export_trace_chunks(&mut self, trace_chunks: Vec<TraceChunk<T>>) {
        let res = self
            .export_operation
            .export_trace_chunks(trace_chunks, &self.trace_exporter)
            .await;
        (self.agent_response_handler)(res);
    }
}

#[async_trait::async_trait]
impl<T: Send + Debug + 'static> Worker for TraceExporterWorker<T> {
    async fn run(&mut self) {
        let Some(TraceExporterRunInput { trace_chunks }) = self.run_input.take() else {
            // TODO: this should never happen if the shared runtime works correctly.
            // is it worth putting a debug_assert?
            return;
        };
        if !trace_chunks.is_empty() {
            self.export_trace_chunks(trace_chunks).await;
        }
        // Always ack, even for empty batches. In synchronous mode this advances
        // last_flush_generation so that any sender waiting on the condvar can unblock.
        if self.config.synchronous_export {
            let _ = self.rx.ack_export();
        }
    }

    async fn initial_trigger(&mut self) {
        #[cfg(feature = "test-utils")]
        {
            // Wait for the agent info to be fetched to get deterministic output when deciding
            // to drop traces or not
            #[allow(clippy::unwrap_used)]
            self.trace_exporter
                .wait_agent_info_ready(Duration::from_secs(5))
                .await
                .unwrap();
        }
        self.trigger().await
    }

    async fn trigger(&mut self) {
        let message = self.rx.receive(self.config.max_flush_interval).await;
        let Ok(trace_chunks) = message else {
            return;
        };
        self.run_input = Some(TraceExporterRunInput { trace_chunks });
    }

    async fn shutdown(&mut self) {
        let _ = self.rx.shutdown_done();
    }

    fn reset(&mut self) {
        let _ = self.rx.reset();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use libdd_shared_runtime::SharedRuntime;

    use crate::trace_buffer::{Export, TraceBuffer, TraceBufferConfig};
    use crate::trace_exporter::agent_response::AgentResponse;
    use crate::trace_exporter::error::TraceExporterError;
    use crate::trace_exporter::TraceExporter;

    use super::{BatchFullError, TraceBufferError};

    struct AssertExporter(
        Box<dyn FnMut(Vec<Vec<()>>) + Send + Sync>,
        Arc<tokio::sync::Semaphore>,
    );

    impl std::fmt::Debug for AssertExporter {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_tuple("AssertExporter").finish()
        }
    }

    impl Export<()> for AssertExporter {
        fn export_trace_chunks<'a>(
            &'a mut self,
            trace_chunks: Vec<super::TraceChunk<()>>,
            _trace_exporter: &'a crate::trace_exporter::TraceExporter,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<AgentResponse, TraceExporterError>> + Send>,
        > {
            (self.0)(trace_chunks);
            self.1.add_permits(1);
            Box::pin(async { Ok(AgentResponse::Unchanged) })
        }
    }

    fn make_buffer(
        assert_export: Box<dyn FnMut(Vec<Vec<()>>) + Send + Sync>,
        cfg: TraceBufferConfig,
    ) -> (
        Arc<SharedRuntime>,
        Arc<tokio::sync::Semaphore>,
        TraceBuffer<()>,
    ) {
        let rt = Arc::new(SharedRuntime::new().unwrap());
        let mut builder = TraceExporter::builder();
        builder.set_shared_runtime(rt.clone());
        let sem: Arc<tokio::sync::Semaphore> = Arc::new(tokio::sync::Semaphore::new(0));
        let (sender, worker) = TraceBuffer::new(
            cfg,
            Box::new(
                |_r: Result<AgentResponse, crate::trace_exporter::error::TraceExporterError>| {},
            ),
            Box::new(AssertExporter(assert_export, sem.clone())),
            builder.build().unwrap(),
        );
        rt.spawn_worker(worker).unwrap();
        (rt, sem, sender)
    }

    #[test]
    fn test_receiver_sender_flush() {
        let (rt, sem, sender) = make_buffer(
            Box::new(|chunks| {
                assert_eq!(chunks.len(), 2);
                let mut lengths = chunks.into_iter().map(|c| c.len()).collect::<Vec<_>>();
                lengths.sort();
                assert_eq!(lengths, &[1, 2]);
            }),
            TraceBufferConfig::default()
                .max_buffered_spans(4)
                .span_flush_threshold(2)
                .max_flush_interval(Duration::from_secs(u32::MAX as u64)),
        );

        std::thread::scope(|s| {
            s.spawn(|| sender.send_chunk(vec![()]));
            s.spawn(|| sender.send_chunk(vec![(), ()]));
        });
        let metrics = sender.queue_metrics().get_metrics();
        assert_eq!(metrics.spans_queued, 3);
        assert_eq!(metrics.spans_dropped_full_buffer, 0);

        let _ = rt.block_on(sem.acquire_many(1)).unwrap().unwrap();
        rt.shutdown(None).unwrap();
        sender.wait_shutdown_done(Duration::from_secs(10)).unwrap();
    }

    #[test]
    fn test_receiver_sender_batch_drop() {
        let (rt, sem, sender) = make_buffer(
            Box::new(|chunks| {
                assert_eq!(chunks.len(), 3);
                for (i, chunk) in chunks.into_iter().enumerate() {
                    assert_eq!(chunk.len(), i + 1);
                }
            }),
            TraceBufferConfig::default()
                .max_buffered_spans(4)
                .span_flush_threshold(3)
                .max_flush_interval(Duration::from_secs(u32::MAX as u64)),
        );

        for i in 1..=3 {
            sender.send_chunk(vec![(); i]).unwrap();
        }

        assert!(matches!(
            sender.send_chunk(vec![(); 4]),
            Err(TraceBufferError::BatchFull(BatchFullError {
                spans_dropped: 4
            }))
        ));

        let metrics = sender.queue_metrics().get_metrics();
        assert_eq!(metrics.spans_queued, 6);
        assert_eq!(metrics.spans_dropped_full_buffer, 4);

        let _ = rt.block_on(sem.acquire_many(1)).unwrap().unwrap();
        rt.shutdown(None).unwrap();
        sender.wait_shutdown_done(Duration::from_secs(10)).unwrap();
    }

    #[test]
    fn test_receiver_sender_timeout() {
        let (rt, sem, sender) = make_buffer(
            Box::new(|chunks| {
                assert_eq!(chunks.len(), 1);
            }),
            TraceBufferConfig::default()
                .max_buffered_spans(4)
                .span_flush_threshold(2)
                .max_flush_interval(Duration::from_millis(1)),
        );
        sender.send_chunk(vec![()]).unwrap();
        let _ = rt.block_on(sem.acquire_many(1)).unwrap().unwrap();
        rt.shutdown(None).unwrap();
        sender.wait_shutdown_done(Duration::from_secs(10)).unwrap();
    }

    #[test]
    fn test_send_after_shutdown() {
        let (rt, _, sender) = make_buffer(
            Box::new(|_| panic!("shouldn't be called after shutdown")),
            TraceBufferConfig::default(),
        );
        rt.shutdown(None).unwrap();

        assert!(matches!(
            sender.send_chunk(vec![()]),
            Err(TraceBufferError::AlreadyShutdown)
        ));
    }

    #[test]
    fn test_synchronous_mode() {
        let (rt, sem, sender) = make_buffer(
            Box::new(|chunks| assert_eq!(chunks.len(), 1)),
            TraceBufferConfig::default()
                .synchronous_export(true)
                .synchronous_export_timeout(Some(Duration::from_secs(1))),
        );
        sender.send_chunk(vec![()]).unwrap();
        let _ = sem.try_acquire_many(1).unwrap();

        sender.send_chunk(vec![()]).unwrap();
        let _ = sem.try_acquire_many(1).unwrap();

        sender.send_chunk(vec![()]).unwrap();
        let _ = sem.try_acquire_many(1).unwrap();

        assert_eq!(sender.queue_metrics().get_metrics().spans_queued, 3);
        rt.shutdown(None).unwrap();
    }

    #[test]
    fn test_force_flush() {
        // Set thresholds high enough that send_chunk alone never triggers a flush,
        // and the timer long enough that it won't fire during the test.
        let (rt, sem, sender) = make_buffer(
            Box::new(|chunks| {
                assert_eq!(chunks.len(), 2);
            }),
            TraceBufferConfig::default()
                .max_buffered_spans(100)
                .span_flush_threshold(100)
                .max_flush_interval(Duration::from_secs(u32::MAX as u64)),
        );

        sender.send_chunk(vec![()]).unwrap();
        sender.send_chunk(vec![(), ()]).unwrap();

        // No flush should have happened yet.
        assert_eq!(sem.available_permits(), 0);

        sender.force_flush().unwrap();
        let _ = rt.block_on(sem.acquire_many(1)).unwrap().unwrap();

        rt.shutdown(None).unwrap();
        sender.wait_shutdown_done(Duration::from_secs(10)).unwrap();
    }

    #[test]
    fn test_worker_reset() {
        let (rt, sem, sender) = make_buffer(
            Box::new(|chunks| assert_eq!(chunks.len(), 1)),
            TraceBufferConfig::default().span_flush_threshold(2),
        );
        sender.send_chunk(vec![()]).unwrap();
        assert_eq!(sem.available_permits(), 0);

        rt.before_fork();
        rt.after_fork_child().unwrap();

        sender.send_chunk(vec![(), ()]).unwrap();
        let _ = rt.block_on(sem.acquire_many(1)).unwrap().unwrap();

        assert_eq!(sender.queue_metrics().get_metrics().spans_queued, 2);
        rt.shutdown(None).unwrap();
    }
}
