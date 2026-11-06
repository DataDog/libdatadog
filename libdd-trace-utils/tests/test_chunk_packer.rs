// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The packers must split a batch into payloads that each match a reference encoding of the
//! chunks they hold, stay within the limit, and hold as many chunks as fit.

use libdd_tinybytes::BytesString;
use libdd_trace_utils::msgpack_decoder;
use libdd_trace_utils::msgpack_encoder::{PushError, v1, v04, v05};
use libdd_trace_utils::span::v04::{SpanBytes, SpanEvent, SpanLink};
use libdd_trace_utils::trace_utils::convert_trace_chunks_v04_to_v05;
use libdd_trace_utils::tracer_metadata::TracerMetadata;
use libdd_trace_utils::tracer_payload::TraceChunks;
use std::collections::HashMap;

type Chunk = Vec<SpanBytes>;

/// What the packing loop needs from each format's packer, plus a reference encoder.
trait Format<'a> {
    type Packer;
    fn packer(&self, limit: usize) -> Self::Packer;
    fn push(packer: &mut Self::Packer, chunk: &'a Chunk) -> Result<(), PushError>;
    fn is_empty(packer: &Self::Packer) -> bool;
    fn encoded_len(packer: &Self::Packer) -> usize;
    fn finish(packer: Self::Packer) -> Vec<u8>;
    fn reference(&self, chunks: &[&Chunk]) -> Vec<u8>;
    /// Panics unless `payload` holds exactly `chunks`.
    fn assert_matches(&self, payload: &[u8], chunks: &[&Chunk]) {
        assert_eq!(payload, self.reference(chunks));
    }
}

struct V04;
struct V1<'m>(&'m TracerMetadata);
struct V05;

impl<'a> Format<'a> for V04 {
    type Packer = v04::ChunkPacker;
    fn packer(&self, limit: usize) -> Self::Packer {
        v04::ChunkPacker::new(limit)
    }
    fn push(packer: &mut Self::Packer, chunk: &'a Chunk) -> Result<(), PushError> {
        packer.push(chunk)
    }
    fn is_empty(packer: &Self::Packer) -> bool {
        packer.is_empty()
    }
    fn encoded_len(packer: &Self::Packer) -> usize {
        packer.encoded_len()
    }
    fn finish(packer: Self::Packer) -> Vec<u8> {
        packer.finish()
    }
    fn reference(&self, chunks: &[&Chunk]) -> Vec<u8> {
        v04::to_vec_from_v04(chunks)
    }
}

impl<'a> Format<'a> for V1<'a> {
    type Packer = v1::ChunkPacker<'a>;
    fn packer(&self, limit: usize) -> Self::Packer {
        v1::ChunkPacker::new(self.0, limit)
    }
    fn push(packer: &mut Self::Packer, chunk: &'a Chunk) -> Result<(), PushError> {
        packer.push(chunk)
    }
    fn is_empty(packer: &Self::Packer) -> bool {
        packer.is_empty()
    }
    fn encoded_len(packer: &Self::Packer) -> usize {
        packer.encoded_len()
    }
    fn finish(packer: Self::Packer) -> Vec<u8> {
        packer.finish()
    }
    fn reference(&self, chunks: &[&Chunk]) -> Vec<u8> {
        v1::to_vec_from_v04(chunks, self.0)
    }
}

impl<'a> Format<'a> for V05 {
    type Packer = v05::ChunkPacker;
    fn packer(&self, limit: usize) -> Self::Packer {
        v05::ChunkPacker::new(limit)
    }
    fn push(packer: &mut Self::Packer, chunk: &'a Chunk) -> Result<(), PushError> {
        packer.push(chunk)
    }
    fn is_empty(packer: &Self::Packer) -> bool {
        packer.is_empty()
    }
    fn encoded_len(packer: &Self::Packer) -> usize {
        packer.encoded_len()
    }
    fn finish(packer: Self::Packer) -> Vec<u8> {
        packer.finish()
    }
    fn reference(&self, chunks: &[&Chunk]) -> Vec<u8> {
        let chunks: Vec<Chunk> = chunks.iter().map(|chunk| (*chunk).clone()).collect();
        let TraceChunks::V05(payload) = convert_trace_chunks_v04_to_v05(&chunks).unwrap() else {
            unreachable!()
        };
        rmp_serde::to_vec(&payload).unwrap()
    }
    // Span maps are hash maps, so compare decoded payloads, with sizes as a byte-level check.
    fn assert_matches(&self, payload: &[u8], chunks: &[&Chunk]) {
        let reference = self.reference(chunks);
        assert_eq!(payload.len(), reference.len());
        assert_eq!(
            msgpack_decoder::v05::from_slice(payload).unwrap().0,
            msgpack_decoder::v05::from_slice(&reference).unwrap().0
        );
    }
}

enum Outcome {
    Payload(Vec<usize>, Vec<u8>),
    Oversized(usize),
}

/// Greedy packing, keeping chunks that fit on either side of an oversized one together.
fn pack<'a, F: Format<'a>>(format: &F, chunks: &'a [Chunk], limit: usize) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    let mut packer = format.packer(limit);
    let mut indices = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        match F::push(&mut packer, chunk) {
            Ok(()) => indices.push(index),
            Err(PushError::Full) if F::is_empty(&packer) => {
                outcomes.push(Outcome::Oversized(index))
            }
            Err(PushError::Full) => {
                let mut next = format.packer(limit);
                match F::push(&mut next, chunk) {
                    Ok(()) => {
                        let full = std::mem::replace(&mut packer, next);
                        let len = F::encoded_len(&full);
                        let bytes = F::finish(full);
                        assert_eq!(bytes.len(), len);
                        outcomes.push(Outcome::Payload(std::mem::take(&mut indices), bytes));
                        indices.push(index);
                    }
                    Err(PushError::Full) => outcomes.push(Outcome::Oversized(index)),
                    Err(error) => panic!("{error}"),
                }
            }
            Err(error) => panic!("{error}"),
        }
    }
    if !indices.is_empty() {
        let len = F::encoded_len(&packer);
        let bytes = F::finish(packer);
        assert_eq!(bytes.len(), len);
        outcomes.push(Outcome::Payload(indices, bytes));
    }
    outcomes
}

/// Checks bounds, order, wire equivalence, oversized classification, and maximality.
fn check<'a, F: Format<'a>>(format: &F, chunks: &'a [Chunk], limit: usize) -> usize {
    let outcomes = pack(format, chunks, limit);
    let mut seen = Vec::new();
    let mut payloads = 0;
    for (position, outcome) in outcomes.iter().enumerate() {
        match outcome {
            Outcome::Oversized(index) => {
                assert!(format.reference(&[&chunks[*index]]).len() > limit);
                seen.push(*index);
            }
            Outcome::Payload(indices, bytes) => {
                payloads += 1;
                assert!(bytes.len() <= limit, "{} > {limit}", bytes.len());
                let selected: Vec<&Chunk> = indices.iter().map(|i| &chunks[*i]).collect();
                format.assert_matches(bytes, &selected);
                // The next payload's first chunk did not fit in this one.
                let next = outcomes[position + 1..].iter().find_map(|o| match o {
                    Outcome::Payload(indices, _) => Some(indices[0]),
                    Outcome::Oversized(_) => None,
                });
                if let Some(next) = next {
                    let mut extended = selected.clone();
                    extended.push(&chunks[next]);
                    assert!(format.reference(&extended).len() > limit);
                }
                seen.extend(indices);
            }
        }
    }
    seen.sort_unstable();
    assert_eq!(seen, (0..chunks.len()).collect::<Vec<_>>());
    payloads
}

fn text(s: impl Into<String>) -> BytesString {
    BytesString::from(s.into())
}

fn span(trace: u64, index: u64, tags: &[(&str, &str)]) -> SpanBytes {
    let mut span = SpanBytes {
        service: text("web"),
        name: text("rack.request"),
        resource: text(format!("GET /items/{}", trace % 7)),
        r#type: text("web"),
        trace_id: u128::from(trace),
        span_id: trace * 100 + index,
        parent_id: if index == 0 { 0 } else { trace * 100 },
        start: 1_700_000_000_000_000_000,
        duration: 1_000 + i64::try_from(index).unwrap(),
        ..Default::default()
    };
    span.meta
        .insert_or_replace(text("http.method"), text("GET"));
    span.meta
        .insert_or_replace(text("unique"), text(format!("value-{trace}-{index}")));
    for (key, value) in tags {
        span.meta.insert_or_replace(text(*key), text(*value));
    }
    span.metrics
        .insert_or_replace(text("_sampling_priority_v1"), 1.0);
    span
}

/// Varied chunks: repeated and unique strings, promoted tags appearing late, span links and
/// events, and a few chunks much larger than the others.
fn chunks() -> Vec<Chunk> {
    // Keep late metadata and link/event cases in the smaller interpreted fixture.
    let (count, large, env_at, version_at) = if cfg!(miri) {
        (6, 4, 1, 3)
    } else {
        (60, 40, 9, 23)
    };
    (0..count)
        .map(|trace| {
            let spans = if trace % 17 == 5 {
                large
            } else if cfg!(miri) {
                1
            } else {
                1 + trace % 4
            };
            (0..spans)
                .map(|index| {
                    let mut tags = Vec::new();
                    if trace == env_at {
                        tags.push(("env", "staging"));
                        tags.push(("_dd.apm_mode", "edge"));
                    }
                    if trace == version_at {
                        tags.push(("version", "1.2.3"));
                        tags.push(("_dd.hostname", "web"));
                    }
                    let mut span = span(trace, index, &tags);
                    if trace % 11 == 3 {
                        span.span_links.push(SpanLink {
                            trace_id: trace,
                            span_id: index,
                            tracestate: text("dd=s:1"),
                            ..Default::default()
                        });
                        span.span_events.push(SpanEvent {
                            time_unix_nano: 1,
                            name: text("exception"),
                            attributes: HashMap::new(),
                        });
                    }
                    span
                })
                .collect()
        })
        .collect()
}

fn metadata() -> TracerMetadata {
    let metadata = TracerMetadata {
        language: "ruby".into(),
        language_version: "3.4.1".into(),
        tracer_version: "2.0.0".into(),
        // Also a span string, so the trailer refers to it by index.
        container_id: "GET".into(),
        // Repeated within the trailer.
        git_commit_sha: "3.4.1".into(),
        ..Default::default()
    };
    metadata
        .mutable_metadata
        .set_runtime_id("2b3e3c64-1f5c-4b5e-9f0a-0d1f4c4f5a6b".into());
    metadata
}

fn limits<'a, F: Format<'a>>(format: &F, chunks: &'a [Chunk]) -> Vec<usize> {
    let all: Vec<&Chunk> = chunks.iter().collect();
    let total = format.reference(&all).len();
    vec![0, 40, 300, 700, 1500, total / 3, total - 1, total]
}

#[test]
fn v04_packs_maximal_equivalent_payloads() {
    let chunks = chunks();
    for limit in limits(&V04, &chunks) {
        check(&V04, &chunks, limit);
    }
}

#[test]
fn v1_packs_maximal_equivalent_payloads() {
    let chunks = chunks();
    let metadata = metadata();
    let format = V1(&metadata);
    for limit in limits(&format, &chunks) {
        check(&format, &chunks, limit);
    }
}

#[test]
fn v05_packs_maximal_equivalent_payloads() {
    let chunks = chunks();
    for limit in limits(&V05, &chunks) {
        check(&V05, &chunks, limit);
    }
}

#[test]
fn whole_batch_fits_in_one_payload() {
    let chunks = chunks();
    let metadata = metadata();
    let all: Vec<&Chunk> = chunks.iter().collect();
    let limit = V1(&metadata).reference(&all).len();
    assert_eq!(check(&V1(&metadata), &chunks, limit), 1);
}

#[test]
fn empty_packers_finish_as_empty_payloads() {
    let metadata = metadata();
    assert_eq!(v04::ChunkPacker::new(0).finish(), V04.reference(&[]));
    assert_eq!(
        v1::ChunkPacker::new(&metadata, 0).finish(),
        V1(&metadata).reference(&[])
    );
    V05.assert_matches(&v05::ChunkPacker::new(0).finish(), &[]);
}

/// The limit binds pushes only: an empty payload is the bare envelope, which can exceed it.
/// `encoded_len` reports that before `finish`, so callers can check.
#[test]
fn empty_payloads_are_not_capped_by_the_limit() {
    let chunk = vec![span(1, 0, &[])];

    let mut packer = v04::ChunkPacker::new(0);
    assert!(matches!(packer.push(&chunk), Err(PushError::Full)));
    assert_eq!(packer.encoded_len(), 1);
    assert_eq!(packer.finish().len(), 1);

    let mut packer = v05::ChunkPacker::new(0);
    assert!(matches!(packer.push(&chunk), Err(PushError::Full)));
    let len = packer.encoded_len();
    assert!(len > 0);
    assert_eq!(packer.finish().len(), len);

    let metadata = TracerMetadata {
        container_id: "c".repeat(1024),
        ..Default::default()
    };
    let limit = 100;
    let mut packer = v1::ChunkPacker::new(&metadata, limit);
    assert!(matches!(packer.push(&chunk), Err(PushError::Full)));
    let len = packer.encoded_len();
    assert!(len > limit);
    assert_eq!(packer.finish().len(), len);
}

/// A limit of exactly the payload size is enough, one byte less is not.
#[test]
fn limits_are_inclusive() {
    let chunk = vec![span(1, 0, &[])];
    let metadata = metadata();
    let len = V1(&metadata).reference(&[&chunk]).len();
    assert!(v1::ChunkPacker::new(&metadata, len).push(&chunk).is_ok());
    assert!(
        v1::ChunkPacker::new(&metadata, len - 1)
            .push(&chunk)
            .is_err()
    );
    let len = V05.reference(&[&chunk]).len();
    assert!(v05::ChunkPacker::new(len).push(&chunk).is_ok());
    assert!(v05::ChunkPacker::new(len - 1).push(&chunk).is_err());
    let len = V04.reference(&[&chunk]).len();
    assert!(v04::ChunkPacker::new(len).push(&chunk).is_ok());
    assert!(v04::ChunkPacker::new(len - 1).push(&chunk).is_err());
}

/// A rejected chunk leaves no strings, nor promoted payload metadata, behind.
#[test]
fn v1_rejected_chunk_is_rolled_back() {
    let metadata = metadata();
    let small = vec![span(1, 0, &[])];
    let large: Chunk = (0..50)
        .map(|index| span(2, index, &[("env", "prod"), ("_dd.apm_mode", "edge")]))
        .collect();
    let later = vec![span(3, 0, &[])];
    let format = V1(&metadata);
    let limit = format.reference(&[&small, &later]).len();

    let mut packer = v1::ChunkPacker::new(&metadata, limit);
    packer.push(&small).unwrap();
    assert!(matches!(packer.push(&large), Err(PushError::Full)));
    packer.push(&later).unwrap();
    assert_eq!(packer.finish(), format.reference(&[&small, &later]));
}

#[test]
fn v05_rejected_chunk_is_rolled_back() {
    let small = vec![span(1, 0, &[])];
    let large: Chunk = (0..50).map(|index| span(2, index, &[])).collect();
    let later = vec![span(3, 0, &[])];
    let limit = V05.reference(&[&small, &later]).len();

    let mut packer = v05::ChunkPacker::new(limit);
    packer.push(&small).unwrap();
    assert!(matches!(packer.push(&large), Err(PushError::Full)));
    packer.push(&later).unwrap();
    V05.assert_matches(&packer.finish(), &[&small, &later]);
}

/// Payload metadata promoted from a span belongs to the payload holding that span only.
#[test]
fn v1_promotes_metadata_per_payload() {
    let metadata = TracerMetadata::default();
    let first = vec![span(1, 0, &[])];
    let second = vec![span(2, 0, &[("env", "staging")])];
    let format = V1(&metadata);
    let limit = format.reference(&[&second]).len();
    let chunks = [first, second];
    let outcomes = pack(&format, &chunks, limit);
    let [Outcome::Payload(a, first), Outcome::Payload(b, second)] = &outcomes[..] else {
        panic!("expected two payloads");
    };
    assert_eq!((a.as_slice(), b.as_slice()), (&[0][..], &[1][..]));
    let decode = |bytes| {
        msgpack_decoder::v1::from_slice(bytes)
            .unwrap()
            .0
            .env
            .to_string()
    };
    assert_eq!(decode(first), "");
    assert_eq!(decode(second), "staging");
}
