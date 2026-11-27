# Remote Configuration reactor

The reactor verifies and stores remote configuration. The tracer supplies HTTP, timers, and cancellation. Products apply the resulting configuration in the tracer runtime.

## Tracer integration builds

The WASM test job uploads the reactor that passed the ABI tests and the signed Agentless test fixture generator. Use the `remote-config-wasm-<sha>` artifact from a trusted workflow run for the source revision under review. Compare `SOURCE_COMMIT` with that revision and check the artifact files with `sha256sum -c SHA256SUMS`. The checksums detect content changes but do not establish trust in the run. This artifact is for integration tests, not release distribution.

## ABI and ownership

Check `rc_version` against `abi.json` before opening a client. Use one serialized caller per reactor instance. Initialize WASI without inherited environment variables or preopened directories.

Input buffers come from `rc_alloc`. Write only within those buffers. Release each buffer with `rc_free` unless the receiving export takes ownership.

Output handles refer to guest-owned data. Use `rc_buffer_ptr` and `rc_buffer_len` to read the bytes, then release the handle. Interpret the pointer as unsigned. Check the range against the current memory size before access. Reacquire memory views after guest calls that can grow memory.

Control records use UTF-8 JSON. HTTP bodies and configuration payloads use separate byte buffers. No payload encoding is necessary at the host boundary.

An export returns `-1` for a contract error. Read `rc_error` before the next failed export. Some exports return `0` for absence, such as an empty event queue or a snapshot that is not ready.

## Poll and cancellation

For Agent polling, supply an HTTP(S) base URL. The session appends `/v0.7/config` to its path.

Call `rc_poll`, then drain `rc_event` until the queue is empty. An HTTP event owns a separate request-body buffer. Release it after reading the request. A sleep event specifies a timer. A cancel event tells the host to stop the corresponding HTTP request or timer.

Return HTTP status and headers with `rc_http_succeed`. That export takes the response-body buffer only when it accepts the completion. Use `rc_http_fail` for transport failures and `rc_timer_elapsed` for elapsed timers. After a completion, drain events again. Operation identifiers are local to the client. Do not log credentials from HTTP events.

Read `rc_result` when available. The result distinguishes polling errors, no change, and an accepted batch. The error diagnostic is limited to 4096 UTF-8 bytes. Use the result interval for the next poll. Apply changes to identity or interests before starting another poll.

`batchFullyWatched: true` means all changes in a nonempty batch belong to live watches when the result is read. The raw view is still readable and must be released. A host that uses only those watches for these products can release the view without describing it. Keep any host-side raw receipt or retarget cleanup. An absent field never permits skipping the raw view.

For Agent sessions, HTTP 204 and 404 report no change. They do not clear the current assignment or its application receipts. Send the original status rather than translating it in the host.

HTTP 404 also keeps any processing error report pending for the next request.

The client sends complete receipt diagnostics when the encoded request fits in the transport buffer. If the request is too large, it omits diagnostic text without changing receipt states. If the other fields still exceed the limit, the poll returns an error without starting an HTTP operation.

Set interests when opening a client or with `rc_set_interest`. Use `capabilities` for capability names. If the tracer already has a bitset, pass its set bit positions as `capabilityIndices` instead. Unknown indices or a request with both nonempty lists are errors.

Use `rc_cancel` to stop a pending poll without destroying the session. Drain cancellation events and read the cancelled result before restarting. When closing a client, stop its host work before releasing the guest client with `rc_close`.

## Target changes

Use `rc_set_identity` to refresh identity and keep the current assignment and receipts. Supply `processTags` to replace the tags. An empty list clears them. Omit the field to retain the current tags.

Use `rc_retarget` with `{ target, identity }` to change the target at the same endpoint. Read any pending poll result before calling `rc_retarget`. In open and retarget requests, `identity.processTags` takes precedence over `target.processTags` when supplied.

`rc_retarget` returns the target generation. Before the first poll, it replaces the initial registration. After the first poll, it clears readiness and makes receipts from the previous target stale. Retained views still describe the previous target generation.

Agentless sessions retain TUF trust. A reply without verified director targets cannot restore readiness after a target change. Open a new client if the endpoint or API key changes.

The tracer owns TLS and verifies the server certificate. The reactor verifies signed Agentless configuration with TUF. The `agentless-custom-transport` Cargo feature keeps that verification without the built-in Rust HTTP client. Existing Rust callers can continue to use `agentless` for the built-in client.

## Assignment and application

A batch retains its own revision of the assignment. Use `rc_view_snapshot` to read that exact assignment. Use `rc_snapshot` to replay the current assignment. A ready empty assignment is different from an uninitialized assignment.

Use `rc_view_describe` for metadata and `rc_view_file` for payloads. An upsert can include a previous revision. Metadata encodes versions and generations as decimal strings. Release a retained view with `rc_view_release`. Release payload handles separately.

The current payload handle from `rc_view_file` is also the application receipt. The receipt remains valid after the host releases its batch. It retains only that file revision.

Call `rc_apply` with the receipt after the product applies the configuration. The export accepts a current receipt and returns `0` for a stale receipt. A removed or previous revision cannot report an outcome. Release the receipt with `rc_free` when application ends.

If the reactor exports `rc_apply_receipts`, raw consumers can report deferred outcomes without keeping a view or payload handle. Keep the raw descriptor’s `receiptOwner` and decimal string `generation` with each observed upsert or snapshot path. Release the payload once the consumer has read it. Keep receipts in the reactor that issued them; watch descriptors do not issue raw receipts.

Between polls, pass a JSON buffer such as `{"receiptOwner":2,"outcomes":[{"path":"...","generation":"1","state":2}]}` to `rc_apply_receipts(client, plan)`. The states are `1` (unacknowledged), `2` (acknowledged), and `3` (error, with an optional `message`). Release the plan buffer after the call. Invalid states or generation encodings reject the plan before any outcome is recorded. The return value counts outcomes recorded for paths still assigned to the same revision and not owned by a watch. Replaced, removed, retargeted, and foreign-client receipts do not count.

Use `rc_apply_plan(client, view, plan)` to complete a group of upserts in one call. The plan starts with one outcome byte: `0` (applied), `1` (retained), or `2` (failed). A failed outcome is followed by a diagnostic. After the outcome, repeat these fields until the end of the buffer:

- The little-endian `u32` index of the upsert in `rc_view_describe`.
- One decision byte: `0` (eligible), `1` (ignored), or `2` (rejected).
- A diagnostic, only for a rejected decision.

A diagnostic is a little-endian `u32` byte length followed by that many UTF-8 bytes. For example, `[0, 1, 0, 0, 0, 0]` applies a plan with one eligible upsert at index 1. Include one decision per upsert in the group, in increasing index order. The export rejects duplicate indices, removals, and snapshots before updating any receipt. The total diagnostic size after expansion cannot exceed 16 MiB per plan.

Submit an applied outcome after the product installs the value, or a retained outcome when it keeps the live value. Both acknowledge eligible files. A failed outcome reports its diagnostic on eligible files. An ignored file is always acknowledged. A rejected file always reports its own diagnostic.

The return value is the number of current receipts updated. Replaced, removed, and foreign-client receipts do not count. Complete between polls. Release the plan buffer and batch view separately. Use deferred receipts for snapshot replay or to report outcomes after releasing the view. Without that export, retain and complete individual payload handles instead.

The tracer owns subscription order, application ownership, and product semantics. A value subscription can recompute settings from an assignment. A task handler must process each observed command without coalescing.

### Typed watches

The host provides product-specific projection and installation. The watch schedules applications and completes receipts. It takes effect on the same session as the low-level poll API. Include its products and capabilities in the session interest.

Call `rc_watch_open(client, options)` with `{ "delivery": "latestState", "products": ["APM_TRACING"] }`. For ordered transitions, use `"orderedState"` and add `"maxPending"` from 1 to 32. Register before or after the first poll. A watch owns completion for its products until it drains. Use the low-level API for commands.

Call `rc_watch_next(client, watch)` between polls. It returns `0` when there is no work, `-2` for ordered overflow, or a positive update view. The descriptor adds `reason`: `initial`, `changed`, `replay`, or `retry`. Changes are relative to the last assignment the watch finished without an installation failure. Use `rc_view_snapshot` on the update to read the complete verified set for its products. These views can read payloads but cannot complete individual receipts. Release each derived snapshot or payload separately. Continue to read poll results and release the low-level batch view.

After installation, call `rc_watch_finish(client, watch, update, plan)` between polls with the compact plan from the previous section. Provide one decision per upsert in the update. The operation consumes the view on success and returns the number of current receipts it completes. Invalid plans leave the view active. Do not release the update with `rc_view_release`. When retargeting invalidates an active install, finish it with an empty decision list to let the next install start.

Watch upserts follow the order in which the session first accepted each revision. Within a response, upserts follow the Agent assignment order. Removals follow all upserts. Use the update descriptor indices for the decision plan.

`rc_watch_replay(client, watch)` queues the current verified value without fetching. It returns `1` for queued work or `0` before readiness. Both replay and dispatch report `-2` for ordered overflow. Recover by closing and reopening the watch or by changing the target. The product chooses how to restore its state after a gap.

The ordered queue counts only payloads for the watch's products. When there is an update to deliver, dispatch needs space for its view. An idle or overflowed watch can still report its status when all views are in use.

`rc_watch_close(client, watch)` returns `1` when the watch is closed or `0` while an install drains. In the second case, finish that update with an empty decision list. Closing does not undo installed settings. Do not close the session until active product operations have stopped.
