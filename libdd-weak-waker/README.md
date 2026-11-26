# libdd-weak-waker

Future wrapper that holds the task waker weakly.

## Overview

When a task spawned on an async runtime waits on a channel, the channel keeps a
clone of the task waker. If the other end of the channel lives outside the
runtime, that waker can keep the runtime scheduler, and its I/O driver, alive
after the runtime is dropped.

`WeakWakerFuture` wraps a future and gives it a waker that points to a slot
owned by the task. When the runtime drops the task, the slot is cleared, so
the real waker and the scheduler it references can be freed.

The crate is `no_std` and only needs `alloc`, so it also builds for wasm.

## Example

```rust
use libdd_weak_waker::WeakWakerFuture;

async fn wait_for_message(mut rx: futures::channel::mpsc::UnboundedReceiver<()>) {
    WeakWakerFuture::new(futures::StreamExt::next(&mut rx)).await;
}
```
