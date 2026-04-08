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
/// Trace-level fields (origin, meta, metrics) are not included here: the trace
/// operation handlers use direct named-field access since they operate on
/// `&mut Trace<T::Text>` references (not raw span pointers).
pub struct SpanFieldTable {
    /// Byte offsets of `T::Text` fields in `Span<T>`, indexed by:
    ///   0 = service, 1 = name, 2 = resource, 3 = type
    pub str_fields: [usize; 4],
    /// Byte offsets of `i32` fields in `Span<T>`, indexed by:
    ///   0 = error
    pub i32_fields: [usize; 1],
    /// Byte offsets of `i64` fields in `Span<T>`, indexed by:
    ///   0 = start, 1 = duration
    pub i64_fields: [usize; 2],
    /// Byte offsets of `HashMap<T::Text, T::Text>` fields in `Span<T>`, indexed by:
    ///   0 = meta
    pub str_map_fields: [usize; 1],
    /// Byte offsets of `HashMap<T::Text, f64>` fields in `Span<T>`, indexed by:
    ///   0 = metrics
    pub f64_map_fields: [usize; 1],
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
            i32_fields: [offset_of!(Span<T>, error)],
            i64_fields: [offset_of!(Span<T>, start), offset_of!(Span<T>, duration)],
            str_map_fields: [offset_of!(Span<T>, meta)],
            f64_map_fields: [offset_of!(Span<T>, metrics)],
        }
    }
}
