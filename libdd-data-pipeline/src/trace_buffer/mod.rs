// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Trace buffer that batches trace chunks and periodically flushes them through a
//! [`TraceExporter`]. A background worker handles the actual export, allowing callers to
//! enqueue traces without blocking on network I/O (unless synchronous mode is enabled).

use std::{
    fmt::{self, Debug},
    ops::DerefMut,
    pin::Pin,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use libdd_capabilities::{HttpClientTrait, MaybeSend};
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
    TimedOut(Duration),
    MutexPoisoned,
    BatchFull(BatchFullError),
    TraceExporter(TraceExporterError),
}

struct Batch<T> {
    chunks: Vec<TraceChunk<T>>,
    last_flush: Instant,
    span_count: usize,
    max_buffered_spans: usize,
    batch_gen: BatchGeneration,
}

// Pre-allocate the batch buffer to avoid reallocations on small sizes.
// A trace chunk is 24 bytes, so this takes 24 * 400 = 9.6kB
const PRE_ALLOCATE_CHUNKS: usize = 400;

impl<T> Batch<T> {
    fn new(max_buffered_spans: usize) -> Self {
        let mut batch_gen = BatchGeneration::default();
        batch_gen.incr();
        Self {
            chunks: Vec::with_capacity(PRE_ALLOCATE_CHUNKS),
            last_flush: Instant::now(),
            span_count: 0,
            batch_gen,
            max_buffered_spans,
        }
    }

    fn reset(&mut self) {
        let Self {
            chunks,
            last_flush,
            span_count,
            batch_gen,
            max_buffered_spans: _max_buffered_spans,
        } = self;
        chunks.clear();
        *last_flush = Instant::now();
        *span_count = 0;

        *batch_gen = {
            let mut batch_gen = BatchGeneration::default();
            batch_gen.incr();
            batch_gen
        };
    }

    fn span_count(&self) -> usize {
        self.span_count
    }

    /// Add a trace chunk to the batch
    /// If the batch is already too big, drop the chunk and return an error
    ///
    /// This method will not check that adding the chunk will not exceed the maximum size of the
    /// batch. So the batch can be over the maximum size after this call.
    /// This is because we don't want to always drop traces that contain more spans than the maximum
    /// size.
    fn add_trace_chunk(&mut self, chunk: Vec<T>) -> Result<(), BatchFullError> {
        if self.span_count > self.max_buffered_spans {
            return Err(BatchFullError {
                spans_dropped: chunk.len(),
            });
        }
        if chunk.is_empty() {
            return Ok(());
        }

        let chunk_len = chunk.len();
        self.chunks.push(chunk);
        self.span_count += chunk_len;
        Ok(())
    }

    /// Export the trace chunk and reset the batch
    fn export(&mut self) -> Vec<TraceChunk<T>> {
        let chunks = std::mem::replace(&mut self.chunks, Vec::with_capacity(PRE_ALLOCATE_CHUNKS));
        self.span_count = 0;
        self.last_flush = Instant::now();
        if !chunks.is_empty() {
            self.batch_gen.incr();
        }
        chunks
    }
}

/// # TraceBuffer
///
/// Creating an instance of the TraceBuffer will spawn a background task that
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
    ) -> (Self, TraceExporterWorker<T>) {
        let (tx, rx) = channel(
            config.span_flush_threshold,
            config.max_buffered_spans,
            config.synchronous_export,
        );
        let worker = TraceExporterWorker::new(rx, response_handler, export_operation, config);
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
        if trace_chunk.is_empty() {
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

    pub fn queue_metrics(&self) -> QueueMetricsFetcher<T> {
        QueueMetricsFetcher {
            waiter: self.tx.waiter.clone(),
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

pub struct QueueMetricsFetcher<T> {
    waiter: Arc<Waiter<T>>,
}

impl<T> QueueMetricsFetcher<T> {
    pub fn get_metrics(&self) -> QueueMetrics {
        let Some(mut state) = self.waiter.state.lock().ok() else {
            return QueueMetrics::default();
        };
        std::mem::take(&mut state.metrics)
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
    let waiter = Arc::new(Waiter {
        state: Mutex::new(SharedState {
            flush_needed: false,
            last_flush_generation: BatchGeneration::default(),
            has_shutdown: false,
            batch: Batch::new(max_number_of_spans),
            metrics: QueueMetrics::default(),
        }),
        sender_notifier: Condvar::new(),
        receiver_notifier: tokio::sync::Notify::new(),
    });
    (
        Sender {
            waiter: waiter.clone(),
            flush_trigger_number_of_spans,
            synchronous_write,
        },
        Receiver { waiter },
    )
}

struct Sender<T> {
    waiter: Arc<Waiter<T>>,
    flush_trigger_number_of_spans: usize,
    synchronous_write: bool,
}

impl<T> Sender<T> {
    fn wait_flush_done(
        &self,
        flush_gen: BatchGeneration,
        timeout: Option<Duration>,
    ) -> Result<(), TraceBufferError> {
        let cond = |state: &mut SharedState<T>| {
            state.last_flush_generation < flush_gen && !state.has_shutdown
        };

        if let Some(timeout) = timeout {
            if timeout.is_zero() {
                return Err(TraceBufferError::TimedOut(Duration::ZERO));
            }
            let state = self.get_state()?;
            let (_state, res) = self
                .waiter
                .sender_notifier
                .wait_timeout_while(state, timeout, cond)
                .map_err(|_| TraceBufferError::MutexPoisoned)?;
            if res.timed_out() {
                return Err(TraceBufferError::TimedOut(timeout));
            }
        } else {
            let state = self.get_state()?;
            let _state = self
                .waiter
                .sender_notifier
                .wait_while(state, cond)
                .map_err(|_| TraceBufferError::MutexPoisoned)?;
        }
        Ok(())
    }

    fn get_state(&self) -> Result<MutexGuard<'_, SharedState<T>>, TraceBufferError> {
        self.waiter
            .state
            .lock()
            .map_err(|_| TraceBufferError::MutexPoisoned)
    }

    fn get_running_state(&self) -> Result<MutexGuard<'_, SharedState<T>>, TraceBufferError> {
        let state = self.get_state()?;
        if state.has_shutdown {
            return Err(TraceBufferError::AlreadyShutdown);
        }
        Ok(state)
    }

    fn add_trace_chunk(&self, chunk: Vec<T>) -> Result<BatchGeneration, TraceBufferError> {
        let mut state = self.get_running_state()?;
        let chunk_len = chunk.len();
        if let Err(e @ BatchFullError { spans_dropped }) = state.batch.add_trace_chunk(chunk) {
            state.metrics.spans_dropped_full_buffer += spans_dropped;
            return Err(TraceBufferError::BatchFull(e));
        }
        state.metrics.spans_queued += chunk_len;
        let gen = state.batch.batch_gen;
        if !state.flush_needed
            && (state.batch.span_count() > self.flush_trigger_number_of_spans
                || self.synchronous_write)
        {
            state.flush_needed = true;
            self.waiter.notify_receiver(state);
        }
        Ok(gen)
    }

    fn trigger_flush(&self) -> Result<(), TraceBufferError> {
        let mut state = self.get_running_state()?;
        state.flush_needed = true;
        self.waiter.notify_receiver(state);
        Ok(())
    }

    fn wait_shutdown_done(&self, timeout: Duration) -> Result<(), TraceBufferError> {
        if timeout.is_zero() {
            return Err(TraceBufferError::TimedOut(Duration::ZERO));
        }
        let state = self.get_state()?;
        let (_state, res) = self
            .waiter
            .sender_notifier
            .wait_timeout_while(state, timeout, |state| !state.has_shutdown)
            .map_err(|_| TraceBufferError::MutexPoisoned)?;
        if res.timed_out() {
            return Err(TraceBufferError::TimedOut(timeout));
        }
        Ok(())
    }
}

struct Receiver<T> {
    waiter: Arc<Waiter<T>>,
}

impl<T> Receiver<T> {
    fn shutdown_done(&self) -> Result<(), MutexPoisonedError> {
        let mut state = self.waiter.state.lock().map_err(|_| MutexPoisonedError)?;
        state.has_shutdown = true;
        self.waiter.notify_sender(state);
        Ok(())
    }

    fn reset(&self) -> Result<(), MutexPoisonedError> {
        let mut state = self.waiter.state.lock().map_err(|_| MutexPoisonedError)?;
        let SharedState {
            flush_needed,
            last_flush_generation,
            has_shutdown,
            batch,
            metrics,
        } = state.deref_mut();
        *flush_needed = false;
        *last_flush_generation = BatchGeneration::default();
        *has_shutdown = false;
        batch.reset();
        *metrics = QueueMetrics::default();
        Ok(())
    }

    async fn receive(&self, timeout: Duration) -> Result<Vec<TraceChunk<T>>, MutexPoisonedError> {
        loop {
            // Enable the notify future BEFORE acquiring the lock to avoid lost wakeups:
            // any notify_waiters() call that fires between enable() and .await is captured.
            let notified = self.waiter.receiver_notifier.notified();
            let mut notified = std::pin::pin!(notified);
            notified.as_mut().enable();

            // The MutexGuard must not be held across .await points
            let leftover;
            {
                let mut state = self.waiter.state.lock().map_err(|_| MutexPoisonedError)?;
                if state.flush_needed {
                    state.flush_needed = false;
                    return Ok(state.batch.export());
                }
                let deadline = state.batch.last_flush + timeout;
                leftover = deadline.saturating_duration_since(Instant::now());
                if leftover == Duration::ZERO {
                    return Ok(state.batch.export());
                }
            } // MutexGuard dropped before any .await

            tokio::select! {
                biased;
                _ = notified.as_mut() => {}  // woken by sender; loop to re-check state
                _ = tokio::time::sleep(leftover) => {
                    let mut state = self.waiter.state.lock().map_err(|_| MutexPoisonedError)?;
                    return Ok(state.batch.export());
                }
            }
        }
    }

    fn ack_export(&self) -> Result<(), MutexPoisonedError> {
        let mut state = self.waiter.state.lock().map_err(|_| MutexPoisonedError)?;
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

struct SharedState<T> {
    flush_needed: bool,
    last_flush_generation: BatchGeneration,
    has_shutdown: bool,
    batch: Batch<T>,
    metrics: QueueMetrics,
}

struct Waiter<T> {
    state: Mutex<SharedState<T>>,
    sender_notifier: Condvar,
    receiver_notifier: tokio::sync::Notify,
}

impl<T> Waiter<T> {
    fn notify_receiver(&self, state: MutexGuard<'_, SharedState<T>>) {
        drop(state);
        self.receiver_notifier.notify_one();
    }

    #[inline(always)]
    fn notify_sender(&self, state: MutexGuard<'_, SharedState<T>>) {
        drop(state);
        self.sender_notifier.notify_all();
    }
}
/// A pluggable export operation for the trace buffer
///
/// This allows mapping from the buffered spans to another type, and
/// calling any export method to send traces.
pub trait Export<T>: Send + Debug {
    fn export_trace_chunks(
        &mut self,
        trace_chunks: Vec<TraceChunk<T>>,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<AgentResponse, TraceExporterError>> + Send + '_,
        >,
    >;

    /// Called once before the first trigger to allow the export operation to perform any
    /// async setup (e.g. waiting for agent info).
    #[cfg(feature = "test-utils")]
    fn wait_ready(
        &mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug)]
pub struct DefaultExport<H: HttpClientTrait + MaybeSend + Sync + 'static> {
    trace_exporter: TraceExporter<H>,
}

impl<H: HttpClientTrait + MaybeSend + Sync + 'static> DefaultExport<H> {
    pub fn new(trace_exporter: TraceExporter<H>) -> Self {
        Self { trace_exporter }
    }
}

impl<H: HttpClientTrait + MaybeSend + Sync + 'static>
    Export<libdd_trace_utils::span::v04::SpanBytes> for DefaultExport<H>
{
    fn export_trace_chunks(
        &mut self,
        trace_chunks: Vec<TraceChunk<libdd_trace_utils::span::v04::SpanBytes>>,
    ) -> Pin<
        Box<
            dyn std::future::Future<Output = Result<AgentResponse, TraceExporterError>> + Send + '_,
        >,
    > {
        Box::pin(async {
            self.trace_exporter
                .send_trace_chunks_async(trace_chunks)
                .await
        })
    }

    #[cfg(feature = "test-utils")]
    fn wait_ready(
        &mut self,
    ) -> Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + '_>> {
        Box::pin(async {
            self.trace_exporter
                .wait_agent_info_ready(Duration::from_secs(5))
                .await
        })
    }
}

#[derive(Debug)]
struct TraceExporterRunInput<T> {
    trace_chunks: Vec<TraceChunk<T>>,
}

pub struct TraceExporterWorker<T> {
    rx: Receiver<T>,
    export_operation: Box<dyn Export<T> + Send + Sync>,
    agent_response_handler: ResponseHandler,
    config: TraceBufferConfig,

    run_input: Option<TraceExporterRunInput<T>>,
}

impl<T: Debug> std::fmt::Debug for TraceExporterWorker<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TraceExporterWorker")
            .field("export_operation", &self.export_operation)
            .field("config", &self.config)
            .field("run_input", &self.run_input)
            .finish()
    }
}

impl<T: Send + 'static> TraceExporterWorker<T> {
    fn new(
        rx: Receiver<T>,
        agent_response_handler: ResponseHandler,
        export_operation: Box<dyn Export<T> + Send + Sync>,
        config: TraceBufferConfig,
    ) -> Self {
        Self {
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
            .export_trace_chunks(trace_chunks)
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
            if let Err(MutexPoisonedError) = self.rx.ack_export() {}
        }
    }

    async fn initial_trigger(&mut self) {
        #[cfg(feature = "test-utils")]
        {
            #[allow(clippy::unwrap_used)]
            self.export_operation.wait_ready().await.unwrap();
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
        fn export_trace_chunks(
            &mut self,
            trace_chunks: Vec<super::TraceChunk<()>>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<AgentResponse, TraceExporterError>>
                    + Send
                    + '_,
            >,
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
        let sem: Arc<tokio::sync::Semaphore> = Arc::new(tokio::sync::Semaphore::new(0));
        let (sender, worker) = TraceBuffer::new(
            cfg,
            Box::new(
                |_r: Result<AgentResponse, crate::trace_exporter::error::TraceExporterError>| {},
            ),
            Box::new(AssertExporter(assert_export, sem.clone())),
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
