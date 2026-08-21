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

use libdd_capabilities::{HttpClientCapability, LogWriterCapability, MaybeSend, SleepCapability};
use libdd_shared_runtime::{SharedRuntime, Worker};
use libdd_trace_utils::span::{
    span_pool::{PooledChunks, SpanPool},
    BytesData,
};

use crate::trace_exporter::{
    agent_response::AgentResponse, error::TraceExporterError, TraceExporter,
};

/// Trait for types stored in a [`TraceBuffer`] that can report their approximate byte size.
pub trait BufferSize {
    fn byte_size(&self) -> usize;
}

impl<T> BufferSize for libdd_trace_utils::span::v04::Span<T>
where
    T: libdd_trace_utils::span::TraceData,
    T::Text: AsRef<str>,
    T::Bytes: AsRef<[u8]>,
{
    fn byte_size(&self) -> usize {
        use libdd_trace_utils::span::v04::AttributeAnyValue;

        // trace_id(16) + span_id(8) + parent_id(8) + start(8) + duration(8) + error(4)
        let mut size: usize = 52;

        size += self.service.as_ref().len();
        size += self.name.as_ref().len();
        size += self.resource.as_ref().len();
        size += self.r#type.as_ref().len();

        // We expect VecMaps to be already deduped at this point, so `defensive_dedup` should be
        // cheap (and alloc-free). In the future we could relax the check and accept non-deduped
        // VecMap, trading over-estimating the size of a span for less work.
        for (k, v) in self.meta.defensive_dedup().iter() {
            size += k.as_ref().len() + v.as_ref().len();
        }
        for (k, _) in self.metrics.defensive_dedup().iter() {
            size += k.as_ref().len() + 8;
        }
        for (k, v) in self.meta_struct.defensive_dedup().iter() {
            size += k.as_ref().len() + v.as_ref().len();
        }
        for link in &self.span_links {
            // trace_id(8) + trace_id_high(8) + span_id(8) + flags(4) = 28
            size += 28 + link.tracestate.as_ref().len();
            for (k, v) in &link.attributes {
                size += k.as_ref().len() + v.as_ref().len();
            }
        }
        for event in &self.span_events {
            // time_unix_nano(8)
            size += 8 + event.name.as_ref().len();
            for (k, v) in &event.attributes {
                size += k.as_ref().len()
                    + match v {
                        AttributeAnyValue::SingleValue(av) => span_attr_size::<T>(av),
                        AttributeAnyValue::Array(vec) => vec.iter().map(span_attr_size::<T>).sum(),
                    };
            }
        }
        size
    }
}

fn span_attr_size<T>(v: &libdd_trace_utils::span::v04::AttributeArrayValue<T>) -> usize
where
    T: libdd_trace_utils::span::TraceData,
    T::Text: AsRef<str>,
{
    use libdd_trace_utils::span::v04::AttributeArrayValue;
    match v {
        AttributeArrayValue::String(s) => s.as_ref().len(),
        AttributeArrayValue::Boolean(_) => 1,
        AttributeArrayValue::Integer(_) => 8,
        AttributeArrayValue::Double(_) => 8,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TraceBufferConfig {
    synchronous_export: bool,
    synchronous_export_timeout: Option<Duration>,
    max_flush_interval: Duration,
    max_buffered_bytes: usize,
    flush_threshold_bytes: usize,
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

    /// The maximum number of bytes that will be buffered before we drop data
    pub fn max_buffered_bytes(self, max: usize) -> Self {
        Self {
            max_buffered_bytes: max,
            ..self
        }
    }

    /// The number of bytes that will be buffered before we decide to flush
    pub fn flush_threshold_bytes(self, threshold: usize) -> Self {
        Self {
            flush_threshold_bytes: threshold,
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
            max_buffered_bytes: 5_000_000,    // 5MB
            flush_threshold_bytes: 1_500_000, // 1.5MB
        }
    }
}

pub type TraceChunk<T> = Vec<T>;

/// Error that can occur when the batch has reached its maximum size
/// and we can't add more data to it.
///
/// The added data will be dropped.
#[derive(Debug, PartialEq, Eq)]
pub struct BatchFullError {
    pub spans_dropped: usize,
}

/// Error that can occur when the mutex was poisoned.
///
/// The only way to handle it is to log and try to return an empty but valid state
#[derive(Debug)]
struct MutexPoisonedError;

#[derive(Debug)]
pub enum TraceBufferError {
    AlreadyClosed,
    TimedOut(Duration),
    MutexPoisoned,
    BatchFull(BatchFullError),
    TraceExporter(TraceExporterError),
}

struct Batch<T> {
    chunks: Vec<TraceChunk<T>>,
    last_flush: Instant,
    byte_count: usize,
    max_buffered_bytes: usize,
    batch_gen: BatchGeneration,
}

// Pre-allocate the batch buffer to avoid reallocations on small sizes.
// A trace chunk is 24 bytes, so this takes 24 * 400 = 9.6kB
const PRE_ALLOCATE_CHUNKS: usize = 400;

impl<T> Batch<T> {
    fn new(max_buffered_bytes: usize) -> Self {
        let mut batch_gen = BatchGeneration::default();
        batch_gen.incr();
        Self {
            chunks: Vec::with_capacity(PRE_ALLOCATE_CHUNKS),
            last_flush: Instant::now(),
            byte_count: 0,
            batch_gen,
            max_buffered_bytes,
        }
    }

    fn reset(&mut self) {
        let Self {
            chunks,
            last_flush,
            byte_count,
            batch_gen,
            max_buffered_bytes: _max_buffered_bytes,
        } = self;
        chunks.clear();
        *last_flush = Instant::now();
        *byte_count = 0;

        *batch_gen = {
            let mut batch_gen = BatchGeneration::default();
            batch_gen.incr();
            batch_gen
        };
    }

    /// Add a trace chunk to the batch
    /// If the batch is already too big, drop the chunk and return an error
    ///
    /// This method will not check that adding the chunk will not exceed the maximum size of the
    /// batch. So the batch can be over the maximum size after this call.
    /// This is because we don't want to always drop traces that contain more bytes than the maximum
    /// size.
    fn add_trace_chunk(&mut self, chunk: Vec<T>) -> Result<(), BatchFullError>
    where
        T: BufferSize,
    {
        if self.byte_count > self.max_buffered_bytes {
            return Err(BatchFullError {
                spans_dropped: chunk.len(),
            });
        }
        if chunk.is_empty() {
            return Ok(());
        }

        self.byte_count += chunk.iter().map(|s| s.byte_size()).sum::<usize>();
        self.chunks.push(chunk);
        Ok(())
    }

    /// Export the trace chunk and reset the batch
    fn export(&mut self) -> Vec<TraceChunk<T>> {
        let chunks = std::mem::replace(&mut self.chunks, Vec::with_capacity(PRE_ALLOCATE_CHUNKS));
        self.byte_count = 0;
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
/// * [`TraceBuffer::flush_and_wait`] is called. This method triggers a flush and waits for the
///   flush to be done before returning
///
/// # Synchronous mode
///
/// If [`TraceBufferConfig::synchronous_writes`] is true, this blocks until
/// * Either until the chunks have been flushed to the agent
/// * Or if `synchronous_writes_timeout` is Some, until the timeout is reached. At which point the
///   flush might continue in the background
pub struct TraceBuffer<T> {
    tx: Sender<T>,
    /// Enables synchronous exports
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

impl<T: Send + BufferSize + 'static> TraceBuffer<T> {
    pub fn new(
        config: TraceBufferConfig,
        response_handler: ResponseHandler,
        export_operation: Box<dyn Export<T> + Send + Sync>,
    ) -> (Self, TraceExporterWorker<T>) {
        let (tx, rx) = channel(
            config.flush_threshold_bytes,
            config.max_buffered_bytes,
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
        self.tx.trigger_flush().map(|_flush_gen| ())
    }

    /// Flush the current batch and wait, up to `timeout`, for the exporter to export it.
    ///
    /// [`TraceBuffer::force_flush`] only waits for the flush request to reach the queue.
    /// `flush_and_wait` leaves the buffer usable, so a caller that exposes a blocking
    /// `flush()` to its own users can send more chunks after `flush_and_wait` returns. Use
    /// [`TraceBuffer::flush_and_close`] instead to also close the buffer.
    ///
    /// An idle buffer has nothing to wait for, so `flush_and_wait` returns immediately.
    ///
    /// A `timeout` of `None` blocks the calling thread until the exporter acks the export or
    /// until the buffer closes. Neither event is guaranteed: a paused worker never acks and
    /// never closes, and a hung agent request holds the export open. Pass a timeout unless
    /// the caller can tolerate an unbounded block.
    ///
    /// # Errors
    ///
    /// * `TimedOut` when the exporter does not ack the export within `timeout`. The buffer stays
    ///   usable, and a later call can wait for the same data again.
    /// * `AlreadyClosed` when the buffer refuses chunks, or when a concurrent close ends the wait
    ///   before the ack arrives. In the second case the outcome of the flush is unknown, because
    ///   the shutdown drain can still export the data.
    pub fn flush_and_wait(&self, timeout: Option<Duration>) -> Result<(), TraceBufferError> {
        self.tx.flush_and_wait(timeout)
    }

    /// Flush the current batch, wait for the exporter to export it, then close the buffer so
    /// no further chunks are accepted.
    ///
    /// `flush_and_close` does not stop the worker that exports the buffer. A caller that owns
    /// the worker (for example through a `libdd_shared_runtime::WorkerHandle`) must stop it
    /// separately.
    ///
    /// [`TraceBuffer::flush_and_wait`] waits for the same export without closing the buffer.
    /// [`TraceBuffer::wait_close_done`] waits on a close that another caller starts.
    /// `flush_and_close` starts the close itself.
    ///
    /// `flush_and_close` closes the buffer even when the flush times out. If the call
    /// returns `Err(TimedOut)`, the exporter may not have exported the flush, but no caller
    /// can queue further chunks after the call returns.
    ///
    /// The error cases of [`TraceBuffer::flush_and_wait`] apply here too, including the
    /// unbounded block on a `timeout` of `None`.
    pub fn flush_and_close(&self, timeout: Option<Duration>) -> Result<(), TraceBufferError> {
        self.tx.flush_and_close(timeout)
    }

    pub fn queue_metrics(&self) -> QueueMetricsFetcher<T> {
        QueueMetricsFetcher {
            waiter: self.tx.waiter.clone(),
        }
    }

    pub fn wait_close_done(&self, timeout: Duration) -> Result<(), TraceBufferError> {
        self.tx.wait_close_done(timeout)
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
    flush_trigger_bytes: usize,
    max_buffered_bytes: usize,
    synchronous_write: bool,
) -> (Sender<T>, Receiver<T>) {
    let waiter = Arc::new(Waiter {
        state: Mutex::new(SharedState {
            flush_needed: false,
            last_flush_generation: BatchGeneration::default(),
            channel_state: ChannelState::Running,
            batch: Batch::new(max_buffered_bytes),
            metrics: QueueMetrics::default(),
        }),
        sender_notifier: Condvar::new(),
        receiver_notifier: tokio::sync::Notify::new(),
    });
    (
        Sender {
            waiter: waiter.clone(),
            flush_trigger_bytes,
            synchronous_write,
        },
        Receiver { waiter },
    )
}

struct Sender<T> {
    waiter: Arc<Waiter<T>>,
    flush_trigger_bytes: usize,
    synchronous_write: bool,
}

impl<T> Sender<T> {
    fn wait_flush_done(
        &self,
        flush_gen: BatchGeneration,
        timeout: Option<Duration>,
    ) -> Result<(), TraceBufferError> {
        let cond = |state: &mut SharedState<T>| {
            state.last_flush_generation < flush_gen && state.channel_state != ChannelState::Stopped
        };

        let state = if let Some(timeout) = timeout {
            let state = self.lock_state()?;
            // A caller that passes a zero timeout asks for a poll, not for a wait. The
            // generation can already be reached, so check it before the timeout wins.
            if state.last_flush_generation >= flush_gen {
                return Ok(());
            }
            if timeout.is_zero() {
                return Err(TraceBufferError::TimedOut(Duration::ZERO));
            }
            let (state, res) = self
                .waiter
                .sender_notifier
                .wait_timeout_while(state, timeout, cond)
                .map_err(|_| TraceBufferError::MutexPoisoned)?;
            if res.timed_out() {
                return Err(TraceBufferError::TimedOut(timeout));
            }
            state
        } else {
            let state = self.lock_state()?;
            self.waiter
                .sender_notifier
                .wait_while(state, cond)
                .map_err(|_| TraceBufferError::MutexPoisoned)?
        };

        // A close also ends the wait, so the generation can still be unreached here. An
        // unreached generation is an error, not a successful flush of data that no exporter took.
        // The outcome stays unknown because a concurrent worker shutdown can still drain and
        // export the data after this point.
        if state.last_flush_generation < flush_gen {
            return Err(TraceBufferError::AlreadyClosed);
        }
        Ok(())
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, SharedState<T>>, TraceBufferError> {
        self.waiter
            .state
            .lock()
            .map_err(|_| TraceBufferError::MutexPoisoned)
    }

    fn get_running_state(&self) -> Result<MutexGuard<'_, SharedState<T>>, TraceBufferError> {
        let state = self.lock_state()?;
        if state.channel_state != ChannelState::Running {
            return Err(TraceBufferError::AlreadyClosed);
        }
        Ok(state)
    }

    fn add_trace_chunk(&self, chunk: Vec<T>) -> Result<BatchGeneration, TraceBufferError>
    where
        T: BufferSize,
    {
        let mut state = self.get_running_state()?;
        let chunk_len = chunk.len();
        if let Err(e @ BatchFullError { spans_dropped }) = state.batch.add_trace_chunk(chunk) {
            state.metrics.spans_dropped_full_buffer += spans_dropped;
            return Err(TraceBufferError::BatchFull(e));
        }
        state.metrics.spans_queued += chunk_len;
        let gen = state.batch.batch_gen;
        if !state.flush_needed
            && (state.batch.byte_count > self.flush_trigger_bytes || self.synchronous_write)
        {
            state.flush_needed = true;
            self.waiter.notify_receiver(state);
        }
        Ok(gen)
    }

    fn trigger_flush(&self) -> Result<BatchGeneration, TraceBufferError> {
        let mut state = self.get_running_state()?;
        let gen = state.batch.batch_gen;
        state.flush_needed = true;
        self.waiter.notify_receiver(state);
        Ok(gen)
    }

    /// Flush the current batch and wait, up to `timeout`, for the exporter to export the data
    /// in the buffer at call time.
    fn flush_and_wait(&self, timeout: Option<Duration>) -> Result<(), TraceBufferError> {
        let mut state = self.get_running_state()?;
        // When no generation is pending, the receiver has nothing to take. A flush request would
        // only wake the receiver for an empty export and reset the auto-flush timer.
        let Some(flush_gen) = state.pending_flush_target() else {
            return Ok(());
        };
        state.flush_needed = true;
        self.waiter.notify_receiver(state);

        self.wait_flush_done(flush_gen, timeout)
    }

    /// Flush and wait as [`Sender::flush_and_wait`] does, then mark the channel as closed so
    /// that the channel refuses further chunks.
    ///
    /// `flush_and_close` marks the channel as closed even when the flush times out. A
    /// caller that tears down the runtime before a fork needs the guarantee that no chunk can
    /// be queued after this call returns, whether or not the flush finished in time.
    fn flush_and_close(&self, timeout: Option<Duration>) -> Result<(), TraceBufferError> {
        let flush_result = self.flush_and_wait(timeout);

        let state = self.lock_state()?;
        self.waiter.mark_stopped(state);

        flush_result
    }

    fn wait_close_done(&self, timeout: Duration) -> Result<(), TraceBufferError> {
        if timeout.is_zero() {
            return Err(TraceBufferError::TimedOut(Duration::ZERO));
        }
        let state = self.lock_state()?;
        let (_state, res) = self
            .waiter
            .sender_notifier
            .wait_timeout_while(state, timeout, |state| {
                state.channel_state != ChannelState::Stopped
            })
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
    fn lock_state(&self) -> Result<MutexGuard<'_, SharedState<T>>, MutexPoisonedError> {
        self.waiter.state.lock().map_err(|_| MutexPoisonedError)
    }

    fn close_done(&self) -> Result<(), MutexPoisonedError> {
        let state = self.lock_state()?;
        self.waiter.mark_stopped(state);
        Ok(())
    }

    fn reset(&self) -> Result<(), MutexPoisonedError> {
        let mut state = self.lock_state()?;
        let SharedState {
            flush_needed,
            last_flush_generation,
            channel_state,
            batch,
            metrics,
        } = state.deref_mut();
        *flush_needed = false;
        *last_flush_generation = BatchGeneration::default();
        *channel_state = ChannelState::Running;
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
                let mut state = self.lock_state()?;
                if state.flush_needed {
                    return Ok(Self::take_batch(&mut state));
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
                    let mut state = self.lock_state()?;
                    return Ok(state.batch.export());
                }
            }
        }
    }

    fn take_batch(state: &mut SharedState<T>) -> Vec<TraceChunk<T>> {
        state.flush_needed = false;
        state.batch.export()
    }

    /// Refuse further chunks and take the chunks currently in the batch, regardless of
    /// `flush_needed`.
    ///
    /// `Worker::shutdown` calls this method because `PausableWorker::pause`'s biased
    /// `select!` cancels the trigger and run loop outright. The loop never hands the batch to
    /// one more `run()` call. Without this drain, `Worker::shutdown` would silently drop the
    /// pending chunks.
    ///
    /// The method refuses chunks and drains under one lock so that this drain is the last one.
    /// A chunk that arrives after the drain but before the export ends would otherwise stay in
    /// a batch that no exporter takes again.
    fn close_and_drain(&self) -> Result<Vec<TraceChunk<T>>, MutexPoisonedError> {
        let mut state = self.lock_state()?;
        state.channel_state = ChannelState::Stopping;
        if state.batch.chunks.is_empty() {
            return Ok(Vec::new());
        }
        Ok(Self::take_batch(&mut state))
    }

    fn ack_export(&self) -> Result<(), MutexPoisonedError> {
        let mut state = self.lock_state()?;
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

    fn decr(&mut self) {
        self.0 = self.0.wrapping_sub(1);
    }
}

/// The lifecycle of a channel: `Running`, `Stopping`, `Stopped`.
///
/// `close_and_drain` sets `Stopping` so that no chunk arrives after the final drain takes
/// the batch. A waiter that treats `Stopping` as `Stopped` reports a successful flush before
/// the drained batch exports. Only `mark_stopped` sets `Stopped`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ChannelState {
    Running,
    Stopping,
    Stopped,
}

struct SharedState<T> {
    flush_needed: bool,
    last_flush_generation: BatchGeneration,
    channel_state: ChannelState,
    batch: Batch<T>,
    metrics: QueueMetrics,
}

impl<T> SharedState<T> {
    /// Return the generation that a caller waits for to confirm that the exporter exported the
    /// data in the buffer now. Return `None` when nothing is left to export.
    ///
    /// The batch increments its generation only for a non-empty export, and the receiver acks
    /// only a non-empty export. A caller that waits on the generation of an empty batch
    /// therefore waits for an ack that never arrives.
    ///
    /// An empty batch means that the receiver already took the chunks, so the export that the
    /// receiver has not acked yet carries the previous generation. The single receiver runs
    /// take, export, and ack in order, so at most one export is ever unacked.
    fn pending_flush_target(&self) -> Option<BatchGeneration> {
        let mut target = self.batch.batch_gen;
        if self.batch.chunks.is_empty() {
            target.decr();
        }
        (self.last_flush_generation < target).then_some(target)
    }
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

    fn mark_stopped(&self, mut state: MutexGuard<'_, SharedState<T>>) {
        state.channel_state = ChannelState::Stopped;
        self.notify_sender(state);
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
pub struct DefaultExport<C, R>
where
    C: HttpClientCapability + SleepCapability + LogWriterCapability + MaybeSend + Sync + 'static,
    R: SharedRuntime + std::fmt::Debug + Send + Sync + 'static,
{
    trace_exporter: TraceExporter<C, R>,
    span_pool: Option<SpanPool<BytesData>>,
}

impl<C, R> DefaultExport<C, R>
where
    C: HttpClientCapability + SleepCapability + LogWriterCapability + MaybeSend + Sync + 'static,
    R: SharedRuntime + std::fmt::Debug + Send + Sync + 'static,
{
    pub fn new(
        trace_exporter: TraceExporter<C, R>,
        span_pool: Option<SpanPool<BytesData>>,
    ) -> Self {
        Self {
            trace_exporter,
            span_pool,
        }
    }
}

impl<C, R> Export<libdd_trace_utils::span::v04::SpanBytes> for DefaultExport<C, R>
where
    C: HttpClientCapability + SleepCapability + LogWriterCapability + MaybeSend + Sync + 'static,
    R: SharedRuntime + std::fmt::Debug + Send + Sync + 'static,
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
                .send_trace_chunks_async(match &self.span_pool {
                    Some(p) => p.wrap_chunks(trace_chunks),
                    None => PooledChunks::unpooled(trace_chunks),
                })
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

    async fn export_and_ack(&mut self, trace_chunks: Vec<TraceChunk<T>>) {
        if trace_chunks.is_empty() {
            return;
        }
        self.export_trace_chunks(trace_chunks).await;
        let _ = self.rx.ack_export();
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
        self.export_and_ack(trace_chunks).await;
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
            // Mailbox mutex is poisoned and unrecoverable. Park forever to avoid a hot loop
            // where the runtime would immediately call trigger() again; the worker will be
            // torn down via its handle / SharedRuntime shutdown.
            tracing::error!("TraceExporterWorker mailbox poisoned; parking until shutdown");
            std::future::pending::<()>().await;
            return;
        };
        self.run_input = Some(TraceExporterRunInput { trace_chunks });
    }

    async fn shutdown(&mut self) {
        if let Ok(trace_chunks) = self.rx.close_and_drain() {
            self.export_and_ack(trace_chunks).await;
        }
        let _ = self.rx.close_done();
    }

    fn reset(&mut self) {
        let _ = self.rx.reset();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use libdd_shared_runtime::{BlockingRuntime, ForkSafeRuntime, SharedRuntime};

    use crate::trace_buffer::{BufferSize, Export, TraceBuffer, TraceBufferConfig};
    use crate::trace_exporter::agent_response::AgentResponse;
    use crate::trace_exporter::error::TraceExporterError;

    use super::{BatchFullError, TraceBufferError};

    // Used for tests, 1 byte per item so size computations are easier
    impl BufferSize for () {
        fn byte_size(&self) -> usize {
            1
        }
    }

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

    /// Park the export future on `gate`. A test holds the worker inside an in-flight export and
    /// acts on the buffer while the export waits on the gate.
    struct GateExporter {
        chunks_handed_to_export: Arc<AtomicUsize>,
        /// A test thread blocks on the receiving end, so an `mpsc::Sender` is the right signal.
        /// `mpsc::Sender` is not `Sync`, and the `Export` trait requires `Send + Sync`.
        export_started: std::sync::Mutex<std::sync::mpsc::Sender<()>>,
        gate: Arc<tokio::sync::Semaphore>,
    }

    impl std::fmt::Debug for GateExporter {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("GateExporter").finish()
        }
    }

    impl Export<()> for GateExporter {
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
            self.chunks_handed_to_export
                .fetch_add(trace_chunks.len(), Ordering::SeqCst);
            if let Ok(tx) = self.export_started.lock() {
                let _ = tx.send(());
            }
            let gate = self.gate.clone();
            Box::pin(async move {
                let _ = gate.acquire().await;
                Ok(AgentResponse::Unchanged)
            })
        }
    }

    fn make_buffer(
        assert_export: Box<dyn FnMut(Vec<Vec<()>>) + Send + Sync>,
        cfg: TraceBufferConfig,
    ) -> (
        Arc<ForkSafeRuntime>,
        Arc<tokio::sync::Semaphore>,
        TraceBuffer<()>,
    ) {
        let rt = Arc::new(ForkSafeRuntime::new().unwrap());
        let sem: Arc<tokio::sync::Semaphore> = Arc::new(tokio::sync::Semaphore::new(0));
        let (sender, worker) = TraceBuffer::new(
            cfg,
            Box::new(
                |_r: Result<AgentResponse, crate::trace_exporter::error::TraceExporterError>| {},
            ),
            Box::new(AssertExporter(assert_export, sem.clone())),
        );
        let _ = rt.spawn_worker(worker, true).unwrap();
        (rt, sem, sender)
    }

    /// A buffer configured to never auto-flush, with a single chunk already sitting in the
    /// batch, for tests exercising shutdown-time draining.
    fn make_buffer_with_pending_chunk() -> (
        Arc<ForkSafeRuntime>,
        Arc<tokio::sync::Semaphore>,
        TraceBuffer<()>,
    ) {
        let (rt, sem, sender) = make_buffer(
            Box::new(|chunks| assert_eq!(chunks.len(), 1)),
            TraceBufferConfig::default()
                .max_buffered_bytes(100)
                .flush_threshold_bytes(100)
                .max_flush_interval(Duration::from_secs(u32::MAX as u64)),
        );
        sender.send_chunk(vec![()]).unwrap();
        (rt, sem, sender)
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_receiver_sender_flush() {
        let (rt, sem, sender) = make_buffer(
            Box::new(|chunks| {
                assert_eq!(chunks.len(), 2);
                let mut lengths = chunks.into_iter().map(|c| c.len()).collect::<Vec<_>>();
                lengths.sort();
                assert_eq!(lengths, &[1, 2]);
            }),
            TraceBufferConfig::default()
                .max_buffered_bytes(4)
                .flush_threshold_bytes(2)
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
        sender.wait_close_done(Duration::from_secs(10)).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_receiver_sender_batch_drop() {
        let (rt, sem, sender) = make_buffer(
            Box::new(|chunks| {
                assert_eq!(chunks.len(), 3);
                for (i, chunk) in chunks.into_iter().enumerate() {
                    assert_eq!(chunk.len(), i + 1);
                }
            }),
            TraceBufferConfig::default()
                .max_buffered_bytes(4)
                .flush_threshold_bytes(3)
                .max_flush_interval(Duration::from_secs(u32::MAX as u64)),
        );

        // pause
        rt.before_fork();

        for i in 1..=3 {
            sender.send_chunk(vec![(); i]).unwrap();
        }

        assert!(matches!(
            sender.send_chunk(vec![(); 4]),
            Err(TraceBufferError::BatchFull(BatchFullError {
                spans_dropped: 4
            }))
        ));

        // unpause
        rt.after_fork_parent().expect("error unpausing");

        let metrics = sender.queue_metrics().get_metrics();
        assert_eq!(metrics.spans_queued, 6);
        assert_eq!(metrics.spans_dropped_full_buffer, 4);

        let _ = rt.block_on(sem.acquire_many(1)).unwrap().unwrap();
        rt.shutdown(None).unwrap();
        sender.wait_close_done(Duration::from_secs(10)).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_receiver_sender_timeout() {
        let (rt, sem, sender) = make_buffer(
            Box::new(|chunks| {
                assert_eq!(chunks.len(), 1);
            }),
            TraceBufferConfig::default()
                .max_buffered_bytes(4)
                .flush_threshold_bytes(2)
                .max_flush_interval(Duration::from_millis(1)),
        );
        sender.send_chunk(vec![()]).unwrap();
        let _ = rt.block_on(sem.acquire_many(1)).unwrap().unwrap();
        rt.shutdown(None).unwrap();
        sender.wait_close_done(Duration::from_secs(10)).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_send_after_shutdown() {
        let (rt, _, sender) = make_buffer(
            Box::new(|_| panic!("shouldn't be called after shutdown")),
            TraceBufferConfig::default(),
        );
        rt.shutdown(None).unwrap();

        assert!(matches!(
            sender.send_chunk(vec![()]),
            Err(TraceBufferError::AlreadyClosed)
        ));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
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
    #[cfg_attr(miri, ignore)]
    fn test_force_flush() {
        // Set thresholds high enough that send_chunk alone never triggers a flush,
        // and the timer long enough that it won't fire during the test.
        let (rt, sem, sender) = make_buffer(
            Box::new(|chunks| {
                assert_eq!(chunks.len(), 2);
            }),
            TraceBufferConfig::default()
                .max_buffered_bytes(100)
                .flush_threshold_bytes(100)
                .max_flush_interval(Duration::from_secs(u32::MAX as u64)),
        );

        sender.send_chunk(vec![()]).unwrap();
        sender.send_chunk(vec![(), ()]).unwrap();

        // No flush should have happened yet.
        assert_eq!(sem.available_permits(), 0);

        sender.force_flush().unwrap();
        let _ = rt.block_on(sem.acquire_many(1)).unwrap().unwrap();

        rt.shutdown(None).unwrap();
        sender.wait_close_done(Duration::from_secs(10)).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_shutdown_flushes_pending_chunk() {
        // `Worker::shutdown` drains any chunk still in the batch before it acks, so the chunk
        // reaches the exporter. `PausableWorker::pause`'s biased `select!` cancels
        // `trigger()` outright instead of letting it hand the batch to one more `run()` call.
        let (rt, sem, sender) = make_buffer_with_pending_chunk();

        rt.shutdown(None).unwrap();
        sender.wait_close_done(Duration::from_secs(10)).unwrap();

        assert_eq!(sem.available_permits(), 1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_flush_and_close_exports_pending_chunk() {
        let (rt, sem, sender) = make_buffer_with_pending_chunk();

        sender
            .flush_and_close(Some(Duration::from_secs(10)))
            .unwrap();

        // flush_and_close returns only after Export::export_trace_chunks handles the
        // chunk buffered at call time and that call returns.
        assert_eq!(sem.available_permits(), 1);

        // flush_and_close refuses a chunk sent after it returns, even though the worker keeps
        // running until a separate caller stops it.
        assert!(matches!(
            sender.send_chunk(vec![()]),
            Err(TraceBufferError::AlreadyClosed)
        ));

        rt.shutdown(None).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_flush_and_close_marks_closed_even_on_timeout() {
        // Pause the worker so the exporter never processes the triggered flush, which
        // guarantees that flush_and_close times out waiting for it.
        let (rt, sem, sender) = make_buffer_with_pending_chunk();
        rt.before_fork();

        assert!(matches!(
            sender.flush_and_close(Some(Duration::from_millis(50))),
            Err(TraceBufferError::TimedOut(_))
        ));

        // A caller that times out still needs the guarantee that no further chunk can sneak
        // in before it tears down the runtime, for example right before a fork.
        assert!(matches!(
            sender.send_chunk(vec![()]),
            Err(TraceBufferError::AlreadyClosed)
        ));

        rt.after_fork_parent().expect("error unpausing");
        rt.shutdown(None).unwrap();

        // Worker::shutdown drains the still-pending chunk and exports it even though the
        // earlier flush_and_close call above timed out waiting for it.
        assert_eq!(sem.available_permits(), 1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_flush_and_close_propagates_already_closed() {
        let (rt, _sem, sender) = make_buffer_with_pending_chunk();
        sender
            .flush_and_close(Some(Duration::from_secs(10)))
            .unwrap();

        assert!(matches!(
            sender.flush_and_close(Some(Duration::from_secs(10))),
            Err(TraceBufferError::AlreadyClosed)
        ));

        rt.shutdown(None).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_flush_and_close_on_idle_buffer_returns_ok_promptly() {
        let (rt, _sem, sender) = make_buffer(
            Box::new(|_| panic!("an idle buffer has nothing to export")),
            TraceBufferConfig::default()
                .max_buffered_bytes(100)
                .flush_threshold_bytes(100)
                .max_flush_interval(Duration::from_secs(u32::MAX as u64)),
        );

        let timeout = Duration::from_millis(200);
        let start = Instant::now();
        let res = sender.flush_and_close(Some(timeout));
        let elapsed = start.elapsed();

        assert!(res.is_ok(), "expected Ok on an idle buffer, got {res:?}");
        assert!(
            elapsed < timeout / 2,
            "flush_and_close blocked for {elapsed:?} with nothing to flush"
        );

        rt.shutdown(None).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_wait_flush_done_does_not_report_ok_for_unexported_data_on_concurrent_close() {
        // Pause the worker so that no exporter ever acks the triggered generation.
        let (rt, _sem, sender) = make_buffer_with_pending_chunk();
        rt.before_fork();

        let flush_gen = sender.tx.trigger_flush().unwrap();

        let res = std::thread::scope(|s| {
            let waiter = s.spawn(|| {
                sender
                    .tx
                    .wait_flush_done(flush_gen, Some(Duration::from_secs(10)))
            });
            std::thread::sleep(Duration::from_millis(50));
            let state = sender.tx.lock_state().unwrap();
            sender.tx.waiter.mark_stopped(state);
            waiter.join().unwrap()
        });

        assert!(
            res.is_err(),
            "wait_flush_done reported success for a generation that was never exported"
        );

        rt.after_fork_parent().expect("error unpausing");
        rt.shutdown(None).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_worker_shutdown_does_not_drop_chunks_sent_during_drain_export() {
        let chunks_handed_to_export = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (export_started_tx, export_started) = std::sync::mpsc::channel();

        let rt = Arc::new(ForkSafeRuntime::new().unwrap());
        let (sender, worker) = TraceBuffer::new(
            TraceBufferConfig::default()
                .max_buffered_bytes(100)
                .flush_threshold_bytes(100)
                .max_flush_interval(Duration::from_secs(u32::MAX as u64)),
            Box::new(|_r: Result<AgentResponse, TraceExporterError>| {}),
            Box::new(GateExporter {
                chunks_handed_to_export: chunks_handed_to_export.clone(),
                export_started: std::sync::Mutex::new(export_started_tx),
                gate: gate.clone(),
            }),
        );
        let _ = rt.spawn_worker(worker, true).unwrap();

        sender.send_chunk(vec![()]).unwrap();

        let shutdown_rt = rt.clone();
        let shutdown = std::thread::spawn(move || shutdown_rt.shutdown(None).unwrap());

        // Land inside Worker::shutdown's drain-then-export window.
        export_started.recv().unwrap();
        let send_during_drain = sender.send_chunk(vec![()]);

        gate.add_permits(1);
        shutdown.join().unwrap();
        sender.wait_close_done(Duration::from_secs(10)).unwrap();

        assert!(
            matches!(send_during_drain, Err(TraceBufferError::AlreadyClosed)),
            "close_and_drain must refuse a chunk sent during the drain export, got \
             {send_during_drain:?}"
        );
        assert_eq!(
            chunks_handed_to_export.load(Ordering::SeqCst),
            1,
            "the drain must export the chunk buffered before the shutdown"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_flush_and_wait_flushes_without_shutting_down() {
        let (rt, sem, sender) = make_buffer_with_pending_chunk();

        sender
            .flush_and_wait(Some(Duration::from_secs(10)))
            .unwrap();
        assert_eq!(sem.available_permits(), 1);

        // A tracer that calls flush() keeps sending traces afterwards.
        sender.send_chunk(vec![()]).unwrap();

        rt.shutdown(None).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_flush_and_wait_waits_for_an_in_flight_export() {
        let chunks_handed_to_export = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (export_started_tx, export_started) = std::sync::mpsc::channel();

        let rt = Arc::new(ForkSafeRuntime::new().unwrap());
        let (sender, worker) = TraceBuffer::new(
            TraceBufferConfig::default()
                .max_buffered_bytes(100)
                .flush_threshold_bytes(0)
                .max_flush_interval(Duration::from_secs(u32::MAX as u64)),
            Box::new(|_r: Result<AgentResponse, TraceExporterError>| {}),
            Box::new(GateExporter {
                chunks_handed_to_export: chunks_handed_to_export.clone(),
                export_started: std::sync::Mutex::new(export_started_tx),
                gate: gate.clone(),
            }),
        );
        let _ = rt.spawn_worker(worker, true).unwrap();

        // The threshold of zero bytes makes the receiver take the batch at once, so the batch is
        // empty while the export is in flight. flush_and_wait must wait for the ack of that
        // export instead of reporting an empty buffer.
        sender.send_chunk(vec![()]).unwrap();
        export_started.recv().unwrap();

        std::thread::scope(|s| {
            let waiter = s.spawn(|| sender.flush_and_wait(Some(Duration::from_secs(10))));
            std::thread::sleep(Duration::from_millis(100));
            assert!(
                !waiter.is_finished(),
                "flush_and_wait returned while the export was still in flight"
            );
            gate.add_permits(1);
            waiter.join().unwrap().unwrap();
        });

        assert_eq!(chunks_handed_to_export.load(Ordering::SeqCst), 1);

        rt.shutdown(None).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_flush_and_wait_leaves_the_buffer_usable_after_a_timeout() {
        let (rt, sem, sender) = make_buffer(
            Box::new(|_| {}),
            TraceBufferConfig::default()
                .max_buffered_bytes(100)
                .flush_threshold_bytes(100)
                .max_flush_interval(Duration::from_secs(u32::MAX as u64)),
        );
        sender.send_chunk(vec![()]).unwrap();

        // Pause the worker so that no exporter acks the flush before the timeout.
        rt.before_fork();
        assert!(matches!(
            sender.flush_and_wait(Some(Duration::from_millis(50))),
            Err(TraceBufferError::TimedOut(_))
        ));

        // A timed-out flush_and_wait keeps the buffer open, unlike flush_and_close.
        sender.send_chunk(vec![()]).unwrap();

        rt.after_fork_parent().expect("error unpausing");
        sender
            .flush_and_wait(Some(Duration::from_secs(10)))
            .unwrap();
        assert!(sem.available_permits() >= 1);

        // Nothing is left to export, so a repeated call returns without a wait.
        sender.flush_and_wait(Some(Duration::ZERO)).unwrap();

        rt.shutdown(None).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_wait_flush_done_polls_an_acked_generation_with_a_zero_timeout() {
        let (rt, sem, sender) = make_buffer_with_pending_chunk();

        let flush_gen = sender.tx.trigger_flush().unwrap();
        sender
            .tx
            .wait_flush_done(flush_gen, Some(Duration::from_secs(10)))
            .unwrap();
        assert_eq!(sem.available_permits(), 1);

        sender
            .tx
            .wait_flush_done(flush_gen, Some(Duration::ZERO))
            .unwrap();

        rt.shutdown(None).unwrap();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_worker_reset() {
        let (rt, sem, sender) = make_buffer(
            Box::new(|chunks| assert_eq!(chunks.len(), 1)),
            TraceBufferConfig::default().flush_threshold_bytes(2),
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
