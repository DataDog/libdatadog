use crate::span::v04::Span;
use crate::span::TraceData;
use std::mem::offset_of;

/// Precomputed byte offsets of `Span<T>` fields, indexed by field_idx as
/// encoded in the upper bits of a simple opcode.
///
/// Built once in `ChangeBufferState::new()` by calling `SpanFieldTable::build::<T>()`.
/// Because `offset_of!` is a compile-time constant at each monomorphization site,
/// the build call is just a handful of `usize` stores — essentially free.
///
/// Only multi-field kinds are represented here. Single-field kinds (i32/error,
/// str_map/meta, f64_map/metrics) are handled by direct field access in
/// `interpret_simple_op` — no offset table needed.
///
/// Trace-level fields (origin, meta, metrics) are not included here: the trace
/// operation handlers use direct named-field access since they operate on
/// `&mut Trace<T::Text>` references (not raw span pointers).
pub struct SpanFieldTable {
    /// Byte offsets of `T::Text` fields in `Span<T>`, indexed by:
    ///   0 = service, 1 = name, 2 = resource, 3 = type
    pub str_fields: [usize; 4],
    /// Byte offsets of `i64` fields in `Span<T>`, indexed by:
    ///   0 = start, 1 = duration
    pub i64_fields: [usize; 2],
}

impl SpanFieldTable {
    /// Build the offset table for a concrete `T: TraceData`.
    ///
    /// Each `offset_of!` call resolves to a compile-time constant at the
    /// monomorphization site, so this is just writing literals into a struct.
    pub fn build<T: TraceData>() -> Self {
        SpanFieldTable {
            str_fields: [
                offset_of!(Span<T>, service),
                offset_of!(Span<T>, name),
                offset_of!(Span<T>, resource),
                offset_of!(Span<T>, r#type),
            ],
            i64_fields: [offset_of!(Span<T>, start), offset_of!(Span<T>, duration)],
        }
    }
}
