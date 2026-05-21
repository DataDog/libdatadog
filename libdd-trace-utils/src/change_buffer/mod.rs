#![allow(unused)]

/// Errors that can occur when operating on a [`ChangeBuffer`] or [`ChangeBufferState`].
#[derive(Debug)]
pub enum ChangeBufferError {
    SpanNotFound(u64),
    StringNotFound(u32),
    ReadOutOfBounds { offset: usize, len: usize },
    WriteOutOfBounds { offset: usize, len: usize },
    UnknownOpcode(u32),
}

impl std::fmt::Display for ChangeBufferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeBufferError::SpanNotFound(id) => write!(f, "span not found: {id}"),
            ChangeBufferError::StringNotFound(id) => {
                write!(f, "string not found internally: {id}")
            }
            ChangeBufferError::ReadOutOfBounds { offset, len } => {
                write!(f, "read out of bounds: offset={offset}, len={len}")
            }
            ChangeBufferError::WriteOutOfBounds { offset, len } => {
                write!(f, "write out of bounds: offset={offset}, len={len}")
            }
            ChangeBufferError::UnknownOpcode(val) => write!(f, "unknown opcode: {val}"),
        }
    }
}

impl std::error::Error for ChangeBufferError {}

pub type Result<T> = std::result::Result<T, ChangeBufferError>;

mod utils;
use utils::*;

mod trace;
pub use trace::*;

mod operation;
use operation::*;

mod buffer;
pub use buffer::*;

pub mod span_header;
pub use span_header::{SpanHeader, SPAN_HEADER_SIZE};

use crate::span::v04::Span;
use crate::span::{SpanText, TraceData};

fn vec_insert<K: PartialEq, V>(vec: &mut Vec<(K, V)>, key: K, value: V) {
    for entry in vec.iter_mut() {
        if entry.0 == key {
            entry.1 = value;
            return;
        }
    }
    vec.push((key, value));
}

fn vec_get<'a, K: PartialEq, V>(vec: &'a [(K, V)], key: &K) -> Option<&'a V> {
    for entry in vec {
        if entry.0 == *key {
            return Some(&entry.1);
        }
    }
    None
}

fn deferred_meta_insert(vec: &mut Vec<(u32, u32)>, key_id: u32, val_id: u32) {
    for entry in vec.iter_mut() {
        if entry.0 == key_id {
            entry.1 = val_id;
            return;
        }
    }
    vec.push((key_id, val_id));
}

fn deferred_metric_insert(vec: &mut Vec<(u32, f64)>, key_id: u32, val: f64) {
    for entry in vec.iter_mut() {
        if entry.0 == key_id {
            entry.1 = val;
            return;
        }
    }
    vec.push((key_id, val));
}
