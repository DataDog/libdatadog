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
    pub const SPAN_ID: usize = 0;
    pub const TRACE_ID_LO: usize = 8;
    pub const TRACE_ID_HI: usize = 16;
    pub const PARENT_ID: usize = 24;
    pub const START: usize = 32;
    pub const DURATION: usize = 40;
    pub const ERROR: usize = 48;
    pub const NAME_ID: usize = 52;
    pub const SERVICE_ID: usize = 56;
    pub const RESOURCE_ID: usize = 60;
    pub const TYPE_ID: usize = 64;
    pub const ACTIVE: usize = 68;
}

/// Size of the header in bytes. Must match the #[repr(C)] layout.
pub const SPAN_HEADER_SIZE: usize = std::mem::size_of::<SpanHeader>();

// Compile-time assertions that the struct layout matches the declared offsets and size.
const _: () = {
    assert!(SPAN_HEADER_SIZE == 72);
    assert!(std::mem::offset_of!(SpanHeader, span_id) == offsets::SPAN_ID);
    assert!(std::mem::offset_of!(SpanHeader, trace_id_lo) == offsets::TRACE_ID_LO);
    assert!(std::mem::offset_of!(SpanHeader, trace_id_hi) == offsets::TRACE_ID_HI);
    assert!(std::mem::offset_of!(SpanHeader, parent_id) == offsets::PARENT_ID);
    assert!(std::mem::offset_of!(SpanHeader, start) == offsets::START);
    assert!(std::mem::offset_of!(SpanHeader, duration) == offsets::DURATION);
    assert!(std::mem::offset_of!(SpanHeader, error) == offsets::ERROR);
    assert!(std::mem::offset_of!(SpanHeader, name_id) == offsets::NAME_ID);
    assert!(std::mem::offset_of!(SpanHeader, service_id) == offsets::SERVICE_ID);
    assert!(std::mem::offset_of!(SpanHeader, resource_id) == offsets::RESOURCE_ID);
    assert!(std::mem::offset_of!(SpanHeader, type_id) == offsets::TYPE_ID);
    assert!(std::mem::offset_of!(SpanHeader, active) == offsets::ACTIVE);
};
