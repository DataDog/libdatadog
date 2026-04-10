# No-Arc-Clone Plan (Revised — April 2026)

## Status Assessment: What Has Already Been Done

The original plan was written against an earlier code state. Since then, a significant portion
of it has been implemented. Here is what is already in place:

| Item | Status |
|---|---|
| `Span<T>.meta` / `.metrics` as `Vec<(K,V)>` instead of `HashMap` | ✓ DONE |
| `ChangeBufferState.spans` as `Vec<Option<Span<T>>>` (slot-indexed) | ✓ DONE |
| String table as `Vec<Option<T::Text>>` (O(1) lookup) | ✓ DONE |
| `deferred_meta: Vec<Vec<(u32, u32)>>` per slot | ✓ DONE |
| `deferred_metrics: Vec<Vec<(u32, f64)>>` per slot | ✓ DONE |
| `SetMetaAttr` / `BatchSetMeta` → write (u32, u32) IDs, no `Arc::clone` | ✓ DONE |
| `SetMetricAttr` / `BatchSetMetric` → write (u32, f64), no `Arc::clone` | ✓ DONE |
| `SpanString = Arc<str>` in pipeline and pipeline-native | ✓ DONE (was never bumpalo) |
| JS side (`native_spans.js`) writes u32 IDs for all string args | ✓ DONE (JS already correct) |

---

## Remaining Hot-Path `Arc::clone` Operations

Despite the above, several eager-resolution sites remain in `flush_change_buffer()` and in
the direct WASM setters. Every call to `get_string_arg` / `get_string_arg_unchecked` is an
`Arc::clone` (or a `T::Text::clone` for any `Clone` implementor):

### In `interpret_operation_cached()` — the tight loop for non-Create ops:
- `SetServiceName`: `span.service = unsafe { self.get_string_arg_unchecked(index) }` → **1 Arc::clone**
- `SetResourceName`: `span.resource = unsafe { self.get_string_arg_unchecked(index) }` → **1 Arc::clone**
- `SetName`:         `span.name    = unsafe { self.get_string_arg_unchecked(index) }` → **1 Arc::clone**
- `SetType`:         `span.r#type  = unsafe { self.get_string_arg_unchecked(index) }` → **1 Arc::clone**

### In `interpret_operation()` — Create variants (once per span):
- `CreateSpan`:     `get_string_arg()` for `name` → **1 Arc::clone**
- `CreateSpanFull`: `get_string_arg()` × 4 for name, service, resource, type → **4 Arc::clones**
- All Create variants: `apply_default_meta()` — N×2 `Arc::clone` for each default tag pair

### In `pipeline/src/lib.rs` — direct WASM setters (bypassing the change buffer):
- `setMetaById`:   `cbs.get_string(key_id)` + `cbs.get_string(val_id)` → **2 Arc::clones**
- `setMetricById`: `cbs.get_string(key_id)` → **1 Arc::clone**

The JS side (`native_spans.js`) is already correct: `queueOp`, `queueCreateSpan`,
`queueCreateSpanFull`, `queueBatchMeta`, and `queueBatchMetrics` all convert strings to u32 IDs
via `getStringId()` before writing to the change buffer. **No JS changes are needed.**

---

## Goal (Unchanged)

Eliminate all `Arc::clone` / `Arc::drop` atomic operations from the hot path
(`flush_change_buffer` loop and direct WASM setters). String resolution happens exactly once
per unique string per span, deferred to `flush_chunk()` time.

---

## Core Change: `InProgressSpan`

Introduce a private `InProgressSpan` struct that stores every per-span value as plain integers.
This struct unifies the three data structures that currently track in-progress span state:
`spans: Vec<Option<Span<T>>>`, `deferred_meta`, and `deferred_metrics`.

```rust
/// Private to change_buffer/mod.rs. Holds all span state as integer IDs
/// so that no T::Text clone is ever needed during flush_change_buffer().
struct InProgressSpan {
    // numeric fields
    trace_id:  u128,
    span_id:   u64,
    parent_id: u64,
    start:     i64,
    duration:  i64,
    error:     i32,

    // header string fields as string table IDs (no T::Text)
    name_id:     u32,
    service_id:  u32,
    resource_id: u32,
    type_id:     u32,

    // tag maps: store IDs, resolve at materialize time
    // Vec instead of HashMap — same as current deferred_meta/metrics
    meta:    Vec<(u32, u32)>,   // (key_id, val_id)
    metrics: Vec<(u32, f64)>,   // (key_id, value)
}
```

Note: `InProgressSpan.meta` / `.metrics` use `Vec<(u32, _)>` (not `HashMap`) exactly as the
existing `deferred_meta`/`deferred_metrics` vecs already do. This is just a consolidation.

---

## Files Changed

### 1. `libdd-trace-utils/src/change_buffer/mod.rs` — MAJOR CHANGES

#### `ChangeBufferState<T>` fields:
```rust
// REMOVE:
spans:             Vec<Option<Span<T>>>,
deferred_meta:     Vec<Vec<(u32, u32)>>,
deferred_metrics:  Vec<Vec<(u32, f64)>>,
span_pool:         Vec<Span<T>>,           // pool for in-progress spans, repurpose below

// ADD:
in_progress: Vec<Option<InProgressSpan>>,  // replaces all three above
span_pool:   Vec<Span<T>>,                 // keep — pool for *materialized* Span<T> at flush time
```

All other fields stay unchanged (`string_table`, `traces`, `default_meta`, `span_headers`,
`str_*` cached strings, etc.).

#### Opcode handler changes in `interpret_operation()` and `interpret_operation_cached()`:

| Opcode | Before | After |
|---|---|---|
| `Create` | `new_span_pooled(); apply_default_meta()` | `new_in_progress_pooled()` — no default meta, no T::Text |
| `CreateSpan` | `get_string_arg()` for name | read u32 name_id directly |
| `CreateSpanFull` | `get_string_arg()` × 4 | read 4 u32 IDs directly |
| `SetServiceName` | `get_string_arg_unchecked()` → `span.service` | `ip.service_id = read_u32()` |
| `SetName` | `get_string_arg_unchecked()` → `span.name` | `ip.name_id = read_u32()` |
| `SetResourceName` | `get_string_arg_unchecked()` → `span.resource` | `ip.resource_id = read_u32()` |
| `SetType` | `get_string_arg_unchecked()` → `span.r#type` | `ip.type_id = read_u32()` |
| `SetMetaAttr` | already `(u32,u32)` → `deferred_meta[slot]` | same, but now → `ip.meta` |
| `BatchSetMeta` | already `(u32,u32)` → `deferred_meta[slot]` | same, but now → `ip.meta` |
| `SetMetricAttr` | already `(u32,f64)` → `deferred_metrics[slot]` | same, but now → `ip.metrics` |
| `BatchSetMetric` | already `(u32,f64)` → `deferred_metrics[slot]` | same, but now → `ip.metrics` |

`deferred_meta_insert` / `deferred_metric_insert` helpers are unchanged — they operate on
`Vec<(u32,_)>` and now take `&mut ip.meta` / `&mut ip.metrics` directly.

Also remove `get_string_arg` and `get_string_arg_unchecked` — no longer needed in
`flush_change_buffer`. They can be kept as private helpers for cold paths
(`SetTraceMetaAttr`, `SetTraceMetricsAttr`, `SetTraceOrigin`).

#### New `materialize_in_progress()`:
```rust
fn materialize_in_progress(
    ip: InProgressSpan,
    string_table: &[Option<T::Text>],
    default_meta: &[(T::Text, T::Text)],
    span_pool: &mut Vec<Span<T>>,
) -> Result<Span<T>> {
    let mut span = new_span_pooled(span_pool, ip.span_id, ip.parent_id, ip.trace_id);
    span.start    = ip.start;
    span.duration = ip.duration;
    span.error    = ip.error;

    // Resolve header string IDs — 4 Arc::clones, happens once per span at flush time
    span.name     = get_str(string_table, ip.name_id)?;
    span.service  = get_str(string_table, ip.service_id)?;
    span.resource = get_str(string_table, ip.resource_id)?;
    span.r#type   = get_str(string_table, ip.type_id).unwrap_or_default();

    // Resolve deferred meta/metrics — Arc::clone per unique tag, at flush time
    for (key_id, val_id) in ip.meta {
        if let (Some(k), Some(v)) = (get_str_opt(string_table, key_id),
                                     get_str_opt(string_table, val_id)) {
            vec_insert(&mut span.meta, k, v);
        }
    }
    for (key_id, val) in ip.metrics {
        if let Some(k) = get_str_opt(string_table, key_id) {
            vec_insert(&mut span.metrics, k, val);
        }
    }

    // Apply default meta — Arc::clone per tag, at flush time only
    for (k, v) in default_meta {
        vec_insert(&mut span.meta, k.clone(), v.clone());
    }

    Ok(span)
}
```

#### `flush_chunk()` changes:
```rust
pub fn flush_chunk(&mut self, slot_indices: Vec<u32>, first_is_local_root: bool)
    -> Result<Vec<Span<T>>>
{
    let mut spans_vec = Vec::with_capacity(slot_indices.len());
    for slot in &slot_indices {
        let ip = self.in_progress
            .get_mut(*slot as usize)
            .and_then(|opt| opt.take())
            .ok_or(ChangeBufferError::SpanNotFound(*slot as u64))?;

        let mut span = materialize_in_progress(
            ip,
            &self.string_table,
            &self.default_meta,
            &mut self.span_pool,
        )?;

        // sampling, chunk tags, process_one_span — unchanged
        ...
        spans_vec.push(span);
    }
    // trace cleanup — unchanged
    Ok(spans_vec)
}
```

Public return type unchanged.

#### Public API changes for `get_span` / `span_mut`:

The callers of `get_span` and `span_mut` are:
- **Getters** in `pipeline/lib.rs`: `getServiceName`, `getResourceName`, `getMetaAttr`,
  `getMetricAttr`, `getError`, `getStart`, `getDuration`, `getType`, `getName`,
  `getTraceMetaAttr`, `getTraceMetricAttr`, `getTraceOrigin`.
- **Setters** in `pipeline/lib.rs`: `setMetaById`, `setMetricById`.

For getters, there are two approaches:
1. **Field-specific accessors** (minimal materialization): `get_span_field_str(slot, field)` and
   `get_span_field_i64(slot, field)` that read numeric fields directly from `InProgressSpan` or
   resolve a single string ID on demand. This avoids allocating a full `Span<T>`.
2. **`resolve_span(slot) -> Result<Span<T>>`** (simpler, slightly more work): materializes the
   full span into a temporary `Span<T>`. Acceptable since all getters are cold-path operations
   called for diagnostic/agent-response purposes, not in the hot path.

**Recommendation**: use `resolve_span()` (option 2) for simplicity. For the numeric fields
(`getError`, `getStart`, `getDuration`) it is possible to add lighter accessors later if
profiling shows them to be performance-sensitive.

Rename `get_span()` → `resolve_span()` (or add `resolve_span()` and deprecate `get_span()`).
`span_mut()` can be removed from the public API of `ChangeBufferState`.

New public methods to add:
```rust
/// Materialize an InProgressSpan into a temporary Span<T> for read-only inspection.
/// Cold path — only called for JS getters, not during flush_change_buffer.
pub fn resolve_span(&self, slot: u32) -> Result<Span<T>> { ... }

/// Get trace_id for a slot without full materialization (needed by getTraceMetaAttr etc.)
pub fn get_span_trace_id(&self, slot: u32) -> Result<u128> {
    self.in_progress.get(slot as usize)
        .and_then(|opt| opt.as_ref())
        .map(|ip| ip.trace_id)
        .ok_or(ChangeBufferError::SpanNotFound(slot as u64))
}

/// Defer a meta tag by ID. Replaces span_mut + vec_insert in direct WASM setters.
pub fn defer_meta(&mut self, slot: u32, key_id: u32, val_id: u32) -> Result<()> {
    let ip = self.in_progress.get_mut(slot as usize)
        .and_then(|opt| opt.as_mut())
        .ok_or(ChangeBufferError::SpanNotFound(slot as u64))?;
    deferred_meta_insert(&mut ip.meta, key_id, val_id);
    Ok(())
}

/// Defer a metric tag by ID. Replaces span_mut + vec_insert in direct WASM setters.
pub fn defer_metric(&mut self, slot: u32, key_id: u32, val: f64) -> Result<()> {
    let ip = self.in_progress.get_mut(slot as usize)
        .and_then(|opt| opt.as_mut())
        .ok_or(ChangeBufferError::SpanNotFound(slot as u64))?;
    deferred_metric_insert(&mut ip.metrics, key_id, val);
    Ok(())
}
```

Remove `materialize_slot()` (its purpose — resolving deferred tags into Span<T> before a getter
call — is now handled entirely by `resolve_span()`).

#### `apply_default_meta` at creation time:
Remove the call to `apply_default_meta()` from all Create opcode handlers. Default meta is now
applied inside `materialize_in_progress()` at `flush_chunk()` time. No `Arc::clone` during
span creation.

#### `recycle_spans`:
Unchanged. Still recycles `Vec<Span<T>>` returned by `flush_chunk()`. The pool stores
materialized spans (not InProgressSpan), reusing their pre-allocated `meta`/`metrics` Vecs.

#### `materialize_header()` (SpanHeader path):
This path creates a `Span<T>` from a `SpanHeader` (JS DataView). It already resolves string
IDs via `get_string()`. It inserts into `in_progress` instead of `spans`. The header fields
(name_id, service_id, etc.) map directly to InProgressSpan fields. Logic is essentially the
same, just writing to `InProgressSpan` instead.

---

### 2. `libdatadog-nodejs/crates/pipeline/src/lib.rs` — SETTER CHANGES

**`setMetaById`** — currently:
```rust
let key = cbs.get_string(key_id)?;        // Arc::clone
let val = cbs.get_string(val_id)?;        // Arc::clone
let span = cbs.span_mut(&slot)?;
vec_insert(&mut span.meta, key, val);
```

After:
```rust
cbs.defer_meta(slot, key_id, val_id)      // 0 Arc::clone
    .map_err(|e| JsValue::from_str(&e.to_string()))?;
```

**`setMetricById`** — currently:
```rust
let key = cbs.get_string(key_id)?;        // Arc::clone
let span = cbs.span_mut(&slot)?;
vec_insert(&mut span.metrics, key, val);
```

After:
```rust
cbs.defer_metric(slot, key_id, val)       // 0 Arc::clone
    .map_err(|e| JsValue::from_str(&e.to_string()))?;
```

**Getters** (`getServiceName`, `getResourceName`, `getMetaAttr`, `getMetricAttr`, `getError`,
`getStart`, `getDuration`, `getType`, `getName`):

Replace `cbs.get_span(slot)?` with `cbs.resolve_span(slot)?`. The resolved `Span<T>` is a
temporary; no caching.

```rust
pub fn get_service_name(&self, slot: u32) -> Result<String, JsValue> {
    self.flush_change_queue()?;
    let span = self.cbs.borrow().resolve_span(slot)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    Ok(span.service.to_string())
}
```

**Trace-field getters** (`getTraceMetaAttr`, `getTraceMetricAttr`, `getTraceOrigin`):
These need the span's `trace_id` without a full materialization. Use `get_span_trace_id()`:
```rust
let trace_id = cbs.get_span_trace_id(slot)
    .map_err(|e| JsValue::from_str(&e.to_string()))?;
```

Remove `materialize_slot()` call from `getMetaAttr` / `getMetricAttr` (now handled inside
`resolve_span()`).

---

### 3. `libdatadog-nodejs/crates/pipeline-native/src/lib.rs` — SAME CHANGES

Same setter/getter pattern as above, replacing `get_span()` / `span_mut()` with `resolve_span()`
/ `defer_meta()` / `defer_metric()` / `get_span_trace_id()`.

---

### 4. `libdd-trace-utils/src/change_buffer/span_header.rs` — MINOR CHANGES

`materialize_header()` currently writes into `self.spans`. After the change it writes into
`self.in_progress`. The string ID fields (name_id, service_id, resource_id, type_id) of
`SpanHeader` map 1:1 to the new `InProgressSpan` fields — no logic changes, just the
destination struct.

---

### 5. `libdd-trace-utils/src/span/v04/mod.rs` — NO CHANGE
### 6. `libdatadog-nodejs/crates/pipeline/src/span_string.rs` — NO CHANGE
### 7. `libdatadog-nodejs/crates/pipeline-native/src/span_string.rs` — NO CHANGE
### 8. JS (`dd-trace-js`, `native_spans.js`) — NO CHANGE

---

## Performance Impact After Changes

| Operation | Current state | After InProgressSpan |
|---|---|---|
| `SetMetaAttr` per op | 0 Arc::clone (already deferred) | 0 Arc::clone |
| `BatchSetMeta` per pair | 0 Arc::clone (already deferred) | 0 Arc::clone |
| `SetServiceName` | 1 Arc::clone | **0 Arc::clone** (store u32) |
| `SetName` | 1 Arc::clone | **0 Arc::clone** |
| `SetResourceName` | 1 Arc::clone | **0 Arc::clone** |
| `SetType` | 1 Arc::clone | **0 Arc::clone** |
| `CreateSpan` | 1 Arc::clone (name) | **0 Arc::clone** |
| `CreateSpanFull` | 4 Arc::clone (name/service/resource/type) | **0 Arc::clone** |
| `apply_default_meta` at create | N×2 Arc::clone | **0 Arc::clone** (deferred to flush) |
| `setMetaById` (WASM direct) | 2 Arc::clone | **0 Arc::clone** |
| `setMetricById` (WASM direct) | 1 Arc::clone | **0 Arc::clone** |
| `flush_chunk` per span | 0 clones (strings already in Span<T>) | 4+ Arc::clone per span (cold, acceptable) |
| `getServiceName` / `getName` etc. | 0 clones (borrow from Span<T>) | 1+ Arc::clone per call (cold path) |

Net: `flush_change_buffer` hot loop and direct WASM setters have **zero atomic operations**.
All `Arc::clone` cost moves to `flush_chunk()` — which runs at most once per serialization
cycle, far less frequently than the per-operation change buffer loop.

---

## What Does NOT Change

- `Span<T>` definition and serialization (external type, untouched)
- `TraceData` / `SpanText` / `SpanBytes` traits
- The change buffer binary protocol (wire format, opcode values)
- `Trace<T>`, `SmallTraceMap<T>`
- JS side of the pipeline (`native_spans.js`, `span_context.js`, etc.)
- The `flush_chunk()` public return type (`Vec<Span<T>>`)
- `recycle_spans()` public API
- `string_table_insert_one` / `string_table_evict_one` public API
- `SetTraceMetaAttr` / `SetTraceMetricsAttr` / `SetTraceOrigin` handling
  (these stay with `get_string_arg()` — rare cold-path operations)

---

## Complexity / Risk

**Complexity**: Medium. The core mechanical change is straightforward:
- One new struct (`InProgressSpan`)
- Replace three fields with one in `ChangeBufferState`
- Update ~8 match arms to write u32s instead of calling `get_string_arg`
- Add `resolve_span` / `defer_meta` / `defer_metric` / `get_span_trace_id`
- Update pipeline/pipeline-native getters/setters (~15 call sites)

The tricky parts:
1. `materialize_header()` — merge logic (currently checks if span already exists by span_id
   scan) needs to work with `InProgressSpan` instead of `Span<T>`.
2. `resolve_span()` must not borrow `string_table` and `in_progress` simultaneously in a way
   that trips the borrow checker. Solution: copy the IDs out first, then resolve.
3. The `span_pool` currently pools `Span<T>`. It can still pool materialized spans for reuse
   by `flush_chunk`. The pool for `InProgressSpan` is cheaper (clearing Vecs of u32 is free
   — no atomic decrements), so a pool for InProgressSpan is optional but straightforward.

**String ID validity**: If a string is evicted from the string table before `flush_chunk()`,
the ID will resolve to `None`. This is the same invariant as before — JS must not evict
strings still referenced by live spans. The error behaviour (silently skip unresolved tags,
or return a `StringNotFound` error) is unchanged.

**Testing**: All existing tests that check `state.get_span(slot)?.service == "..."` will need
updating to use `state.resolve_span(slot)?.service == "..."` or to call `flush_chunk()` first.
The invariant they test is unchanged; only the API surface differs.
