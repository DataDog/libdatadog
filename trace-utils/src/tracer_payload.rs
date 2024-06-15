// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::cmp::Ordering;

use crate::trace_utils::cmp_send_data_payloads;
use datadog_trace_protobuf::pb::{Span, TracerPayload};

pub type TracerPayloadV04 = Vec<Span>;

#[derive(Debug, Clone)]
pub enum TraceEncoding {
    V04,
    V07,
}

#[derive(Debug, Clone)]
pub enum TracerPayloadCollection {
    V07(Vec<TracerPayload>),
    V04(Vec<TracerPayloadV04>),
}

impl TracerPayloadCollection {
    pub fn append(&mut self, other: &mut Self) {
        match self {
            TracerPayloadCollection::V07(dest) => {
                if let TracerPayloadCollection::V07(src) = other {
                    dest.append(src)
                }
            }
            TracerPayloadCollection::V04(dest) => {
                if let TracerPayloadCollection::V04(src) = other {
                    dest.append(src)
                }
            }
        }
    }

    pub fn merge(&mut self) {
        if let TracerPayloadCollection::V07(collection) = self {
            collection.sort_unstable_by(cmp_send_data_payloads);
            collection.dedup_by(|a, b| {
                if cmp_send_data_payloads(a, b) == Ordering::Equal {
                    // Note: dedup_by drops a, and retains b.
                    b.chunks.append(&mut a.chunks);
                    return true;
                }
                false
            })
        }
    }

    pub fn size(&self) -> usize {
        match self {
            TracerPayloadCollection::V07(collection) => {
                collection.iter().map(|s| s.chunks.len()).sum()
            }
            TracerPayloadCollection::V04(collection) => collection.iter().map(|s| s.len()).sum(),
        }
    }

    //TODO: not really sure about this.
    pub fn max(&self) -> usize {
        match self {
            TracerPayloadCollection::V07(collection) => {
                collection.iter().map(|s| s.chunks.len()).max().unwrap()
            }
            _ => 0,
        }
    }
}
