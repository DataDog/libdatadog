// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! pprof (`perftools.profiles.Profile`), as the Datadog intake takes it.
//!
//! Trace context becomes labels: `span id` and `local root span id` (as
//! numbers), and `trace endpoint` when the local root span has one. The
//! trace id has no place in pprof and is dropped. Timestamped samples get an
//! `end_timestamp_ns` label.

use super::proto::Writer;
use super::{EncodeError, EncodeOptions, Numbering, Period, StringTable};
use crate::collections::alloc::Global;
use crate::collections::vec::TryVec;
use crate::profile::{Frame, Function, LabelValue, Profile, Resolve, Stacks, Str};

/// Encodes `profile`, which ends at `end_ns` (Unix nanoseconds).
///
/// # Errors
///
/// See [`EncodeError`].
#[allow(
    clippy::too_many_lines,
    reason = "one pass that mirrors the message layout"
)]
pub fn encode<S, F, K>(
    profile: &Profile<S, F, K>,
    end_ns: i64,
    period: Option<Period<'_>>,
    options: EncodeOptions,
) -> Result<TryVec<u8, Global>, EncodeError>
where
    S: Resolve<Str>,
    F: Resolve<Function>,
    K: Stacks,
{
    let strings_source = profile.strings();
    let resolve = |id| strings_source.resolve(id).unwrap_or("");
    let mut strings = StringTable::new()?;
    let mut locations = Numbering::<Frame>::new();
    let mut functions = Numbering::<crate::intern_table::FunctionId>::new();
    let mut w = Writer::new(options.max_bytes);

    for sample_type in profile.sample_types() {
        let (name, unit) = (resolve(sample_type.name), resolve(sample_type.unit));
        let (name, unit) = (strings.index(name)?, strings.index(unit)?);
        w.message(1, |w| {
            w.uint64(1, name)?;
            w.uint64(2, unit)
        })?;
    }

    let period = match period {
        Some(p) => Some((strings.index(p.name)?, strings.index(p.unit)?, p.value)),
        None => None,
    };

    let span_id = strings.index("span id")?;
    let local_root = strings.index("local root span id")?;
    let endpoint_key = strings.index("trace endpoint")?;
    let timestamp_key = strings.index("end_timestamp_ns")?;
    let mut ids = TryVec::new_in(Global);
    let mut labels = TryVec::new_in(Global);
    for (key, values, timestamp) in profile.samples() {
        ids.clear();
        labels.clear();
        for frame in profile.stacks().frames(key.stack).unwrap_or(&[]) {
            let id = locations.index(*frame)? + 1;
            ids.try_push(id).map_err(|_| EncodeError::OutOfMemory)?;
        }
        let endpoint = profile
            .endpoint(key.local_root_span_id())
            .map(|endpoint| strings.index(endpoint))
            .transpose()?;
        for label in profile.labels(key.labels) {
            let key = strings.index(resolve(label.key))?;
            let value = match label.value {
                LabelValue::Str(s) => (strings.index(resolve(s))?, 0, 0),
                LabelValue::Local(s) => (strings.index(profile.local_string(s))?, 0, 0),
                LabelValue::Num { value, unit } => (0, value, strings.index(resolve(unit))?),
            };
            labels
                .try_push((key, value))
                .map_err(|_| EncodeError::OutOfMemory)?;
        }
        let link = profile.link_at(key.link);
        w.message(2, |w| {
            w.packed_varints(1, ids.iter().copied())?;
            w.packed_varints(2, values.iter().map(|v| v.cast_unsigned()))?;
            for &(key, (s, num, unit)) in &labels {
                label(w, key, s, num, unit)?;
            }
            if !link.is_empty() {
                label(w, span_id, 0, link.span_id_u64().cast_signed(), 0)?;
            }
            if key.local_root_span_id() != 0 {
                label(w, local_root, 0, key.local_root_span_id().cast_signed(), 0)?;
            }
            if let Some(endpoint) = endpoint {
                label(w, endpoint_key, endpoint, 0, 0)?;
            }
            if timestamp != 0 {
                label(w, timestamp_key, 0, timestamp, 0)?;
            }
            Ok(())
        })?;
    }

    let functions_source = profile.functions();
    for (index, frame) in locations.iter().enumerate() {
        let function = functions.index(frame.function)? + 1;
        w.message(4, |w| {
            w.uint64(1, index as u64 + 1)?;
            w.uint64(3, frame.address)?;
            w.message(4, |w| {
                w.uint64(1, function)?;
                w.int64(2, frame.line)
            })
        })?;
    }

    for (index, &id) in functions.iter().enumerate() {
        let function = functions_source.resolve(id).unwrap_or_default();
        let name = strings.index(resolve(function.name))?;
        let system_name = strings.index(resolve(function.system_name))?;
        let filename = strings.index(resolve(function.filename))?;
        w.message(5, |w| {
            w.uint64(1, index as u64 + 1)?;
            w.uint64(2, name)?;
            w.uint64(3, system_name)?;
            w.uint64(4, filename)?;
            w.int64(5, i64::from(function.start_line))
        })?;
    }

    for text in strings.iter() {
        w.bytes(6, text.as_bytes())?;
    }
    w.int64(9, profile.start_ns())?;
    w.int64(10, end_ns.saturating_sub(profile.start_ns()))?;
    if let Some((name, unit, value)) = period {
        w.message(11, |w| {
            w.uint64(1, name)?;
            w.uint64(2, unit)
        })?;
        w.int64(12, value)?;
    }
    Ok(w.finish())
}

fn label(w: &mut Writer, key: u64, s: u64, num: i64, unit: u64) -> Result<(), EncodeError> {
    w.message(3, |w| {
        w.uint64(1, key)?;
        w.uint64(2, s)?;
        w.int64(3, num)?;
        w.uint64(4, unit)
    })
}
