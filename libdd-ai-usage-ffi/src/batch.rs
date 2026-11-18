// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Projection of one observation, and the batch that collects projections
//! over an export window.

use std::sync::Mutex;

use libdd_ai_usage::{
    DatadogProjection, Document, Issues, MetricBatch, RequestDocument, metric_list_from_json,
    pack_dogstatsd_joined, split_constant_tags,
};
use libdd_common_ffi::{CharSlice, Handle, StringWrapper, ToInner};
use serde_json::json;

use crate::{
    AiUsageError, Failure, catch_panic, error_of, json_text, panicked, panicked_output,
    parse_document, shared, text, write_output,
};

/// Projections collected over one export window. Safe to share between
/// threads.
pub struct Batch {
    inner: Mutex<MetricBatch>,
    /// The metric list applied to every document that does not state its
    /// own.
    metrics: Mutex<Option<Vec<String>>>,
}

/// The batch a handle holds.
///
/// # Safety
///
/// `batch` must be null or point to a handle from
/// [`ddog_ai_usage_batch_new`].
unsafe fn batch_of<'a>(batch: *const Handle<Batch>) -> Result<&'a Batch, Failure> {
    // SAFETY: forwarded from the caller.
    unsafe { shared(batch) }.ok_or(Failure::Internal("the batch is null or was released"))
}

fn poisoned<T>(_: T) -> Failure {
    Failure::Internal("the batch is poisoned")
}

/// Project the request document `input` into its Datadog API series and
/// write `{"series": [{"name", "type", "interval", "value", "tags"}],
/// "issues": [...]}` to `out`.
///
/// A Counter is `"type": "rate"` with `"interval": 10`, its value the total
/// divided by 10: what the Agent stores for the same Counter sent over
/// DogStatsD. A Histogram sample is `"type": "distribution"`, with no
/// `interval`. Submit each series with exactly that type, interval and value.
///
/// The document is `{"profile": "{id}@{version}", "observation": {...},
/// "deployment_attributes": {...}, "metrics": [...]}`; the last two are
/// optional.
///
/// # Safety
///
/// `input` must be a valid slice, and `out` must be null or point to a
/// `ddog_StringWrapper` the caller releases with `ddog_StringWrapper_drop`.
/// `out` is written whether the call fails or not (empty when it fails).
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_project(
    input: CharSlice,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe {
            write_output(out, || {
                let document = parse_document(input)?;
                let projection = DatadogProjection::from_request_json(&document)?;
                json_text(serde_json::to_value(&projection))
            })
        },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// Create an empty [`Batch`] and write its handle to `out`. Release it with
/// [`ddog_ai_usage_batch_drop`].
///
/// # Safety
///
/// `out` must be null or point to memory a handle can be written to.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_batch_new(
    out: *mut Handle<Batch>,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        {
            if out.is_null() {
                return Some(Failure::Internal("the output is null").into());
            }
            let batch = Batch {
                inner: Mutex::new(MetricBatch::new()),
                metrics: Mutex::new(None),
            };
            // SAFETY: `out` is not null and the caller guarantees it is
            // writable.
            unsafe { out.write(Handle::from(batch)) };
            None
        },
        panicked()
    )
}

/// Add every observation of one request to `batch` together: `input` is a
/// JSON array of request documents, each `{"profile", "observation",
/// "deployment_attributes", "metrics"}` as [`ddog_ai_usage_project`] reads
/// it (its provider attempts, token breakdowns and gateway request).
/// Either all are added or none: a bounded batch without room for all of
/// them drops them all, so it never keeps an attempt without its breakdown,
/// and counts each in [`ddog_ai_usage_batch_dropped`]. A rejected document
/// rejects the request, adds nothing, and returns an error whose message
/// names the document's index. On success writes `{"issues": [[...], ...],
/// "dropped": false}` (one issue list per document) to `out`, or nothing when
/// `out` is null.
///
/// # Safety
///
/// `batch` must be a handle from [`ddog_ai_usage_batch_new`], `input` a valid
/// slice, and `out` null or as [`ddog_ai_usage_project`] takes it.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_batch_add_request(
    batch: *const Handle<Batch>,
    input: CharSlice,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        {
            let add = || {
                // SAFETY: forwarded from the caller.
                let batch = unsafe { batch_of(batch) }?;
                let request = parse_document(input)?;
                // Decoded before the batch is locked; projected once it is.
                let decoded = RequestDocument::from_request_json(&request)?;
                let defaults = batch.metrics.lock().map_err(poisoned)?.clone();
                let defaults: Option<Vec<&str>> = defaults
                    .as_ref()
                    .map(|names| names.iter().map(String::as_str).collect());
                let documents: Vec<Document<'_>> = decoded
                    .iter()
                    .map(|document| document.document(defaults.as_deref()))
                    .collect();
                add_documents(batch, &documents)
            };
            if out.is_null() {
                return error_of(add());
            }
            // SAFETY: forwarded from the caller.
            unsafe {
                write_output(out, || {
                    let (issues, added) = add()?;
                    Ok(json!({
                        "issues": issues
                            .iter()
                            .map(|issues| {
                                issues.iter().map(|issue| issue.as_str()).collect::<Vec<_>>()
                            })
                            .collect::<Vec<_>>(),
                        "dropped": !added,
                    })
                    .to_string())
                })
            }
        },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// Add a request's documents to `batch`: their issues and whether they were
/// added.
fn add_documents(
    batch: &Batch,
    documents: &[Document<'_>],
) -> Result<(Vec<Issues>, bool), Failure> {
    let mut inner = batch.inner.lock().map_err(poisoned)?;
    let (added, issues) = inner
        .try_add_request(documents)
        .map_err(|error| Failure::Rejected(error.into()))?;
    Ok((issues, added))
}

/// Set the metric list `input` (a JSON array of names, as a request
/// document's `metrics` states it) that `batch` keeps of every document
/// stating none. Parsed once here rather than in every document.
///
/// **An empty list `[]` keeps no metric**, as `"metrics": []` in a document
/// does: every observation added then projects to nothing. `null` or an
/// empty slice keeps every metric again.
///
/// # Safety
///
/// `batch` must be a handle from [`ddog_ai_usage_batch_new`] and `input` a
/// valid slice.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_batch_set_metrics(
    batch: *const Handle<Batch>,
    input: CharSlice,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        error_of((|| {
            // SAFETY: forwarded from the caller.
            let batch = unsafe { batch_of(batch) }?;
            let metrics = if input.as_raw_parts().1 == 0 {
                None
            } else {
                let names = parse_document(input)?;
                metric_list_from_json(&names)?
                    .map(|names| names.into_iter().map(str::to_string).collect())
            };
            *batch.metrics.lock().map_err(poisoned)? = metrics;
            Ok(())
        })()),
        panicked()
    )
}

/// Whether `batch` holds no points. A null or released batch is empty.
///
/// # Safety
///
/// `batch` must be null or a handle from [`ddog_ai_usage_batch_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_batch_is_empty(batch: *const Handle<Batch>) -> bool {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe { batch_of(batch) }.map_or(true, |batch| {
            batch
                .inner
                .lock()
                .map(|inner| inner.is_empty())
                .unwrap_or(true)
        }),
        true
    )
}

/// The number of points `batch` holds: one per Counter series and one per
/// Histogram sample. For a flush threshold, not a line count: the
/// DogStatsD lines taken can be more (each Histogram series also writes a
/// `.count` and a `.sum` line) or fewer (series whose tags normalize alike
/// share their lines). A null or released batch holds none.
///
/// # Safety
///
/// `batch` must be null or a handle from [`ddog_ai_usage_batch_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_batch_len(batch: *const Handle<Batch>) -> usize {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe { batch_of(batch) }.map_or(0, |batch| {
            batch.inner.lock().map(|inner| inner.len()).unwrap_or(0)
        }),
        0
    )
}

/// Take every point of `batch`, leaving it empty, and write it as DogStatsD
/// datagrams of at most `max_payload` bytes ([`crate::UDP_MAX_PAYLOAD`] or
/// [`crate::UDS_MAX_PAYLOAD`]), as text: each datagram's lines joined by
/// `\n`, datagrams separated by a NUL byte, no trailing NUL. `constant_tags`
/// is a comma-separated list of `key:value` tags added to every line, such as
/// the client's `env`; it may be empty. A line longer than `max_payload` with
/// its tags goes alone in a datagram of its own; only one longer than
/// [`crate::MAX_LINE_BYTES`] (or `max_payload`, if larger) is dropped, and the
/// number dropped is written to `dropped_lines` when it is not null. An empty
/// batch writes an empty string.
///
/// # Safety
///
/// `batch` must be a handle from [`ddog_ai_usage_batch_new`],
/// `constant_tags` a valid slice, `dropped_lines` null or writable, and `out`
/// as [`ddog_ai_usage_project`] takes it.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_batch_take_dogstatsd_text(
    batch: *const Handle<Batch>,
    constant_tags: CharSlice,
    max_payload: usize,
    dropped_lines: *mut usize,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe {
            write_output(out, || {
                let batch = batch_of(batch)?;
                let tags =
                    text(constant_tags).ok_or(Failure::Internal("constant_tags is not UTF-8"))?;
                let tags = split_constant_tags(tags);
                let lines = batch
                    .inner
                    .lock()
                    .map_err(poisoned)?
                    .take()
                    .dogstatsd_lines()?;
                let (text, dropped) = pack_dogstatsd_joined(
                    lines.iter().map(String::as_str),
                    &tags,
                    max_payload,
                    '\0',
                );
                if !dropped_lines.is_null() {
                    // SAFETY: the caller guarantees `dropped_lines` is
                    // writable.
                    dropped_lines.write(dropped);
                }
                Ok(text)
            })
        },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// Bound `batch` to at most `max_len` points (as
/// [`ddog_ai_usage_batch_len`] counts them), or lift the bound with 0. An
/// observation that would take the batch past its bound is dropped whole
/// and counted by [`ddog_ai_usage_batch_dropped`]. Points already held are
/// kept; taking the batch keeps the bound. A null or released batch is a
/// no-op.
///
/// # Safety
///
/// `batch` must be null or a handle from [`ddog_ai_usage_batch_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_batch_set_max_len(
    batch: *const Handle<Batch>,
    max_len: usize,
) {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        if let Ok(batch) = unsafe { batch_of(batch) }
            && let Ok(mut inner) = batch.inner.lock()
        {
            inner.set_max_len((max_len != 0).then_some(max_len));
        },
        ()
    )
}

/// The observations `batch` dropped because it was full, since it was
/// created. Taking the batch does not reset it, so a caller reports the
/// difference since its last flush. A null or released batch dropped none.
///
/// # Safety
///
/// `batch` must be null or a handle from [`ddog_ai_usage_batch_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_batch_dropped(batch: *const Handle<Batch>) -> u64 {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe { batch_of(batch) }.map_or(0, |batch| {
            batch.inner.lock().map(|inner| inner.dropped()).unwrap_or(0)
        }),
        0
    )
}

/// Tag every series [`ddog_ai_usage_batch_take_intake`] takes from `batch`
/// with this writer's `host.name` (`host`, normalized as Datadog stores a tag
/// value; empty leaves it out) and `process.pid` (`pid`), so replicas and
/// processes never share a series. A null or released batch, or a host that
/// is not UTF-8, is a no-op.
///
/// # Safety
///
/// `batch` must be null or a handle from [`ddog_ai_usage_batch_new`], and
/// `host` a valid slice.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_batch_set_writer(
    batch: *const Handle<Batch>,
    host: CharSlice,
    pid: u32,
) {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        if let (Ok(batch), Some(host)) = (unsafe { batch_of(batch) }, text(host))
            && let Ok(mut inner) = batch.inner.lock()
        {
            inner.set_writer(host, pid);
        },
        ()
    )
}

/// Take every point of `batch`, leaving it empty, as Datadog metrics intake
/// takes it from a writer that submits to the API, and write
/// `{"timestamp", "interval", "dropped", "series": [{"metric", "type",
/// "interval", "tags", "value"}], "distributions": [{"metric", "tags",
/// "sketch"}]}` to `out`:
///
/// - `series` are RATE points over 10 s (`"type": "rate"`, `"interval": 10`, API v2 type 2): the
///   sum since the last take divided by 10, to submit with exactly that type and interval and
///   aggregation SUM. The `.count` and `.sum` companions of distributions are among them. The
///   interval is 10 s whatever the time between takes, the interval the Agent stores DogStatsD
///   counts with, so a metric written both ways keeps one type; `as_count()` reads a point back as
///   the sum since the last take. Taking every 10 s also gives the Agent's per-second values.
/// - `distributions` hold one sketch per series, `sketch` being the base64 of intake's
///   `sketchpb.Sparse` protobuf (keys and counts with min, max, count and sum), as metrics intake
///   computes a DDSketch (relative accuracy 1/128, 4096 bins).
/// - Every point is stamped at `timestamp`: `now_unix_seconds` (0 for the system clock), or the
///   second after this batch's previous take when that is later, so one writer never writes a
///   series twice at the same second.
/// - `tags` are sorted and carry the writer's tags (see [`ddog_ai_usage_batch_set_writer`]).
/// - `dropped` is the observations the batch dropped since the last take.
///
/// # Safety
///
/// `batch` must be a handle from [`ddog_ai_usage_batch_new`], and `out` as
/// [`ddog_ai_usage_project`] takes it.
#[must_use]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_batch_take_intake(
    batch: *const Handle<Batch>,
    now_unix_seconds: i64,
    out: *mut StringWrapper,
) -> Option<Box<AiUsageError>> {
    catch_panic!(
        // SAFETY: forwarded from the caller.
        unsafe {
            write_output(out, || {
                let batch = batch_of(batch)?;
                let mut inner = batch.inner.lock().map_err(poisoned)?;
                let taken = if now_unix_seconds == 0 {
                    inner.take_intake_now()?
                } else {
                    inner.take_intake(now_unix_seconds)?
                };
                json_text(serde_json::to_value(&taken))
            })
        },
        // SAFETY: forwarded from the caller.
        unsafe { panicked_output(out) }
    )
}

/// Release a batch and leave its handle null, so releasing it again is a
/// no-op. A null handle is a no-op.
///
/// # Safety
///
/// `batch` must be null or a handle from [`ddog_ai_usage_batch_new`], not in
/// use by another thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_ai_usage_batch_drop(mut batch: *mut Handle<Batch>) {
    // A null or released handle is an error to take; there is nothing to
    // release.
    catch_panic!(
        if !batch.is_null() {
            // SAFETY: forwarded from the caller.
            drop(unsafe { batch.take() });
        },
        ()
    )
}
