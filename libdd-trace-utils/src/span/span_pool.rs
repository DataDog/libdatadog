use super::v04::Span;
use super::TraceData;
use rand::Rng;

/// When this function returns true, do not add the returned span to the queue.
///
/// Why are we doing this?
///
/// If we keep recylcing spans forever, two things are going to happen
/// * We will keep the **maximum** number of spans ever used by the program alive,
///   even if memory usage scales down
/// * As spans get reused and atributes data structure are pushed and popped, they
///   will tend to grow to have the maximum size of attributes
///
/// Dropping a fixed pct of spans returned ensures that we eventually free memory
/// if span usage spikes, and then goes down.
fn drop_policy() -> bool {
    const PCT_OF_SPANS_RETURNED_DROPPED: f64 = 0.1;
    rand::thread_rng().gen_bool(PCT_OF_SPANS_RETURNED_DROPPED)
}

/// Reset the spans fields to their default value without
/// freeing the the capacity of the collections
fn reset_span<T: TraceData>(
    Span {
        service,
        name,
        resource,
        r#type,
        trace_id,
        span_id,
        parent_id,
        start,
        duration,
        error,
        meta,
        metrics,
        meta_struct,
        span_links,
        span_events,
    }: &mut Span<T>,
) {
    *service = Default::default();
    *name = Default::default();
    *resource = Default::default();
    *r#type = Default::default();
    *trace_id = Default::default();
    *span_id = Default::default();
    *parent_id = Default::default();
    *start = Default::default();
    *duration = Default::default();
    *error = Default::default();
    meta.clear();
    metrics.clear();
    meta_struct.clear();
    span_links.clear();
    span_events.clear();
}

#[derive(Debug)]
pub struct SpanPool<T: TraceData> {
    queue: crossbeam_channel::Sender<Span<T>>,
    receiver: crossbeam_channel::Receiver<Span<T>>,
}

impl<T: TraceData> SpanPool<T> {
    pub fn add_spans<I: IntoIterator<Item = Span<T>>>(&self, spans: I) {
        for mut span in spans {
            if !drop_policy() {
                reset_span(&mut span);
                let _ = self.queue.send(span);
            }
        }
    }

    pub fn get_span(&self) -> Span<T> {
        self.receiver.try_recv().unwrap_or_default()
    }
}

pub struct PooledChunks<'a, T: TraceData> {
    chunks: Vec<Vec<Span<T>>>,
    pool: Option<&'a SpanPool<T>>,
}

impl<'a, T: TraceData> Drop for PooledChunks<'a, T> {
    fn drop(&mut self) {
        if let Some(pool) = self.pool {
            let chunks = std::mem::take(&mut self.chunks);
            pool.add_spans(chunks.into_iter().flatten());
        }
    }
}
