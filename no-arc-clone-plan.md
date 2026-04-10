# No-Arc-Clone Plan (Hybrid Approach)

## Goal

Eliminate `Arc::clone` / `Arc::drop` atomic operations from the hot path (span construction during `flush_change_buffer`). Restore `Arc<str>` in the string table so eviction actually frees memory. Spans store raw u32 string IDs during construction and resolve them to `T::Text` once at `flush_chunk()` time.

## Core Idea

Introduce a new private `InProgressSpan` struct inside `ChangeBufferState`. While a span is being built (opcodes streaming in from the change buffer), `ChangeBufferState` holds `InProgressSpan` values — no `T::Text` anywhere. Only when `flush_chunk()` is called do we materialize each `InProgressSpan` into a `Span<T>` by cloning the needed `Arc<str>` values from the string table exactly once per string per span.

`Span<T>` in `libdd-trace-utils/src/span/v04/mod.rs` is **not changed**. It remains the external, serializable type.

---

## New `InProgressSpan` Struct

Location: private, inside `libdd-trace-utils/src/change_buffer/mod.rs`.

```rust
struct InProgressSpan {
    // numeric fields (unchanged)
    trace_id:  u128,
    span_id:   u64,
    parent_id: u64,
    start:     i64,
    duration:  i64,
    error:     i32,

    // header string IDs (already this way in SpanHeader, just inlining)
    name_id:     u32,
    service_id:  u32,
    resource_id: u32,
    type_id:     u32,

    // tag maps: key ID → value ID (no Arc anywhere)
    meta:    HashMap<u32, u32>,
    // metric maps: key ID → f64 (key is an ID, not T::Text)
    metrics: HashMap<u32, f64>,
}
```

---

## Files Changed

### 1. `libdd-trace-utils/src/change_buffer/mod.rs` — MAJOR CHANGES

**`ChangeBufferState<T>` fields:**
- `spans: FxHashMap<u64, Span<T>>` → `spans: FxHashMap<u64, InProgressSpan>`
- `span_pool: Vec<Span<T>>` → `span_pool: Vec<InProgressSpan>`

**Hot path — opcode handlers (`flush_change_buffer` loop):**
- `SetMetaAttr`, `BatchSetMeta`, `SetService`, etc. currently call `get_string_arg()` which does `Arc::clone`.
- After the change: read the u32 ID directly with `get_num_arg::<u32>()` and store it into `InProgressSpan.meta`/`.metrics`. No `Arc::clone`.
- `get_string_arg()` can be removed entirely (or kept only for the few cold-path string reads like trace-level meta).

**New `materialize_in_progress(span: InProgressSpan, table: &[Option<T::Text>]) -> Result<Span<T>>`:**
- Called once per span inside `flush_chunk()`.
- Looks up each u32 ID in the string table and clones `T::Text` (one `Arc::clone` per unique string, at flush time, not at write time).
- Applies `default_meta` (language, runtime-id, etc.) here instead of at span creation time.

**`flush_chunk()` changes:**
- Iterates over the span IDs in the chunk, calls `materialize_in_progress()` for each, collects `Vec<Span<T>>`.
- Returns `Vec<Span<T>>` as before — public API unchanged.

**Public API complications — `span_mut` / `get_span`:**

Currently these return `&mut Span<T>` / `&Span<T>`, but internal storage is now `InProgressSpan`. The public API must change.

Options:
- Remove `span_mut` / `get_span` entirely if no external caller needs mid-construction access.
- Rename to `get_in_progress_span` returning `&InProgressSpan` (exposes IDs, not strings).
- Add a `resolve_span(span_id: u64) -> Result<Span<T>>` that materializes on demand (cold path, acceptable).

The getter methods in `lib.rs` (`get_service_name`, `get_meta_attr`, `get_metric_attr`) are the only known callers of `get_span`. These are cold-path operations. The solution: add `resolve_span(&u64) -> Result<Span<T>>` to `ChangeBufferState<T>` that materializes an `InProgressSpan` into a temporary `Span<T>` for inspection. No caching needed.

**`recycle_spans` (if it exists):**
- Clearing `InProgressSpan` is free: `HashMap<u32, u32>` drop is trivially cheap (no Arc refcount decrements). Pooling is less important but still valid.

### 2. `libdd-trace-utils/src/change_buffer/trace.rs` — NO CHANGE

Trace-level meta (`Trace<T>`) uses `Arc::clone` only when trace origin/meta is set, which is rare. Not worth the complexity.

### 3. `libdd-trace-utils/src/span/v04/mod.rs` — NO CHANGE

`Span<T>` is the external serializable type. Unchanged.

### 4. `libdd-trace-utils/src/change_buffer/span_header.rs` — NO CHANGE

`SpanHeader` already stores u32 IDs (`name_id`, `service_id`, `resource_id`, `type_id`). The `materialize_header()` pattern is exactly what we extend to `meta`/`metrics`. We can inline those fields into `InProgressSpan` directly and remove `SpanHeader` if desired, but it's not required.

### 5. `libdatadog-nodejs/crates/pipeline/src/span_string.rs` — REVERT TO `Arc<str>`

Remove the bumpalo arena. Revert `SpanString` to:

```rust
pub struct SpanString(Arc<str>);

impl Clone for SpanString {
    fn clone(&self) -> Self { SpanString(Arc::clone(&self.0)) }
}
impl From<String> for SpanString {
    fn from(s: String) -> Self { SpanString(Arc::from(s)) }
}
// etc.
```

Remove the `thread_local! { static ARENA: Bump }` block. Remove `bumpalo` from `Cargo.toml`.

### 6. `libdatadog-nodejs/crates/pipeline-native/src/span_string.rs` — REVERT TO `Arc<str>`

Same revert. Remove bumpalo. Keep `unsafe impl Send + Sync` only if needed for other reasons (check).

### 7. `libdatadog-nodejs/crates/pipeline/src/lib.rs` — CALLER CHANGES

- Opcode handlers that set meta/metrics (`SetMetaAttr`, `BatchSetMeta`, etc.) no longer call `string_table_insert_key_value(key_id, val_id)` — instead the IDs are written into the change buffer and decoded in `flush_change_buffer()` on the Rust side. No change needed in JS-facing API here; the change is entirely inside `ChangeBufferState`.
- `get_meta_attr` / `get_metric_attr` / `get_service_name`: call `cbs.resolve_span(&span_id)` instead of `cbs.get_span(&span_id)`.

### 8. `libdatadog-nodejs/crates/pipeline-native/src/lib.rs` — CALLER CHANGES

Same getter changes: `resolve_span` instead of `get_span`.

---

## Performance Impact

| Operation | Before (`Arc<str>` in spans) | After (`InProgressSpan`) |
|---|---|---|
| `SetMetaAttr` per op | 2 × `Arc::clone` (atomic fetch_add) | 2 × u32 write (register op) |
| `BatchSetMeta` per pair | 2 × `Arc::clone` | 2 × u32 write |
| `span.meta.clear()` on recycle | N × `Arc::drop` (atomic fetch_sub) | N × u32 drop (no-op) |
| `flush_chunk()` per unique tag | 0 clones (already in span) | 2 × `Arc::clone` (resolution) |
| String eviction | drops `Arc`, may free | same — `Arc` freed when refcount hits 0 |
| Getter (`get_meta_attr`) | 0 clones (read-only borrow) | materializes temporary `Span<T>` (N clones, cold path) |

Net: the hot path (opcode replay during `flush_change_buffer`) has **zero atomic operations**. The cost is paid once at `flush_chunk()` time, which runs far less frequently than the change buffer loop.

---

## What Does NOT Change

- `Span<T>` definition and serialization
- `TraceData` / `SpanText` traits
- The change buffer binary protocol (opcodes, wire format)
- `Trace<T>`, `SmallTraceMap<T>`
- The JS side of the pipeline (no protocol changes)
- The `flush_chunk()` public return type (`Vec<Span<T>>`)

---

## Risk / Complexity

- **Medium complexity**: `InProgressSpan` is a straightforward new type; the hard part is updating all match arms in `flush_change_buffer()` to write IDs instead of cloned strings, and wiring up `materialize_in_progress()`.
- **Getter cold path cost**: `resolve_span()` allocates a temporary `Span<T>`. Acceptable since getters are called only for diagnostic/agent-response purposes, not in the hot path.
- **String ID validity**: At `materialize_in_progress()` time, a string ID may have been evicted. This is an existing invariant (the JS side must not evict a string still referenced by a live span). The error handling is unchanged.
