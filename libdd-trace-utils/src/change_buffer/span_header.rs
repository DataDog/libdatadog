// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::mem;

/// Fixed-layout span header for direct JS DataView access.
///
/// JS creates a DataView over each span's header in WASM linear memory and writes fields directly.
///
/// Strings are interned. String fields store string table IDs (u32) resolved to real strings at
/// flush time. Numeric fields are stored directly.
///
/// Meta/metrics tags are NOT in the header. They still go through the
/// change buffer protocol since their count varies per span.
#[repr(C)]
#[derive(Default, Clone)]
pub struct SpanHeader {
    pub span_id: u64,     // offset 0
    pub trace_id_lo: u64, // offset 8
    pub trace_id_hi: u64, // offset 16
    pub parent_id: u64,   // offset 24
    pub start: i64,       // offset 32
    pub duration: i64,    // offset 40
    pub error: i32,       // offset 48
    pub name_id: u32,     // offset 52
    pub service_id: u32,  // offset 56
    pub resource_id: u32, // offset 60
    pub type_id: u32,     // offset 64
    /// Index into ChangeBufferState.spans for meta/metrics overflow data.
    /// Set when the span is allocated.
    pub active: u32, // offset 68 (1 = in use, 0 = free)
}

/// Field offsets for JS DataView access.
pub mod offsets {
    use super::mem;
    use super::SpanHeader;

    pub const SPAN_ID: usize = mem::offset_of!(SpanHeader, span_id);
    pub const TRACE_ID_LO: usize = mem::offset_of!(SpanHeader, trace_id_lo);
    pub const TRACE_ID_HI: usize = mem::offset_of!(SpanHeader, trace_id_hi);
    pub const PARENT_ID: usize = mem::offset_of!(SpanHeader, parent_id);
    pub const START: usize = mem::offset_of!(SpanHeader, start);
    pub const DURATION: usize = mem::offset_of!(SpanHeader, duration);
    pub const ERROR: usize = mem::offset_of!(SpanHeader, error);
    pub const NAME_ID: usize = mem::offset_of!(SpanHeader, name_id);
    pub const SERVICE_ID: usize = mem::offset_of!(SpanHeader, service_id);
    pub const RESOURCE_ID: usize = mem::offset_of!(SpanHeader, resource_id);
    pub const TYPE_ID: usize = mem::offset_of!(SpanHeader, type_id);
    pub const ACTIVE: usize = mem::offset_of!(SpanHeader, active);
}

/// Size of the header in bytes. Must match the #[repr(C)] layout.
pub const SPAN_HEADER_SIZE: usize = mem::size_of::<SpanHeader>();
