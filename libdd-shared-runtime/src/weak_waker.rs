// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! # Goal
//!
//! This module solves a very specific problem.
//!
//! The problem happens when we use a queue, or any async message passing type in Rust,
//! where one end of the pipe is in a task spawned in an async runtime and the other end
//! is kept outside of the async runtime.
//!
//! When we `await` on the end of the pipe inside of the task, a waker is passed to the
//! `Future` object, and shared with the other end of the pipe outside of the async
//! runtime.
//!
//! The waker needs to keep a reference to the async runtime scheduler so that the user
//! outside of the task can notify the async scheduler to poll the task again.
//!
//! Whenever we want to drop the async runtime, if the task is suspended on one end of
//! the pipe, the task will be dropped but the waker has been shared between both ends
//! of the pipe and can keep the async runtime alive for longer than needed, preventing
//! resources from being freed.
//!
//! When a future that has been wrapped by [`WeakWakerFuture`] is `await`-ed the following
//! happens:
//! * the waker passed to the future is wrapped in a level of indirection, that allows dropping the
//!   original waker without coordinating with the end of the pipe outside of the runtime.
//! * a reference to the wrapper waker is stored inside of the task, so that if the task is dropped,
//!   we drop the original waker.
//!
//! When the async runtime holding the task is dropped, the task will be dropped which
//! will free the original waker, allowing the async runtime's scheduler to be dropped
//! too.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;

use futures_util::task::{waker_ref, ArcWake, AtomicWaker};

/// Wraps an [`AtomicWaker`] to create our own waker.
///
/// [`AtomicWaker`] is in essence an `Option<Waker>`, which allows us to drop the
/// reference to the original whenever the task that needs to be woken is dropped.
struct WeakWakerInner {
    waker: AtomicWaker,
}

impl ArcWake for WeakWakerInner {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        arc_self.waker.wake();
    }
}

struct WeakWaker {
    inner: Arc<WeakWakerInner>,
}

impl Drop for WeakWaker {
    fn drop(&mut self) {
        // Drop the stored reference to the original waker so that resources held by
        // the original waker (e.g. the async runtime scheduler) can be released.
        self.inner.waker.take();
    }
}

pub struct WeakWakerFuture<F: Future> {
    fut: F,
    weak_waker: Option<WeakWaker>,
}

impl<F: Future> WeakWakerFuture<F> {
    /// Wrap a future so that the waker passed to it is held only weakly.
    ///
    /// See the [module-level documentation](self) for details.
    #[must_use = "futures do nothing unless you `.await` or poll them"]
    pub fn new(fut: F) -> WeakWakerFuture<F> {
        WeakWakerFuture {
            fut,
            weak_waker: None,
        }
    }
}

impl<F: Future> Future for WeakWakerFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> std::task::Poll<Self::Output> {
        // SAFETY:
        // Neither `weak_waker` nor `fut` are going to be moved out of `self`.
        let m = unsafe { self.get_unchecked_mut() };

        // On the first poll, allocate the WeakWakerInner Arc.
        // On subsequent polls, reuse it and update the stored waker in-place via
        // AtomicWaker::register — no heap allocation.
        let inner = if let Some(ref ww) = m.weak_waker {
            ww.inner.waker.register(cx.waker());
            &ww.inner
        } else {
            let w = AtomicWaker::new();
            w.register(cx.waker());
            &m.weak_waker
                .insert(WeakWaker {
                    inner: Arc::new(WeakWakerInner { waker: w }),
                })
                .inner
        };

        // SAFETY: structural pinning for `fut`. The shared borrow of `m.weak_waker`
        // and the mutable borrow of `m.fut` are on disjoint fields; NLL allows this.
        unsafe { Pin::new_unchecked(&mut m.fut).poll(&mut Context::from_waker(&waker_ref(inner))) }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::task::Context;

    use super::*;
    use futures::task::waker;
    use futures_util::task::ArcWake;

    struct TestWaker {
        waked: AtomicBool,
    }

    impl ArcWake for TestWaker {
        fn wake_by_ref(s: &Arc<Self>) {
            s.waked.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn test_mpsc_queue_weak_waiter_drop_correctly() {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();

        let mut fut = WeakWakerFuture::new(futures::StreamExt::next(&mut rx));
        let pinned_fut = Pin::new(&mut fut);
        let base_waker = Arc::new(TestWaker {
            waked: AtomicBool::new(false),
        });
        assert!(pinned_fut
            .poll(&mut Context::from_waker(&waker(base_waker.clone())))
            .is_pending());
        assert_eq!(Arc::strong_count(&base_waker), 2);
        drop(fut);
        assert!(!base_waker.waked.load(Ordering::SeqCst));
        assert_eq!(Arc::strong_count(&base_waker), 1);
        tx.unbounded_send(()).unwrap();
    }

    #[test]
    fn test_mpsc_queue_weak_waiter_smoke() {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();

        let mut pinned_fut = pin!(WeakWakerFuture::new(futures::StreamExt::next(&mut rx)));
        let base_waker = Arc::new(TestWaker {
            waked: AtomicBool::new(false),
        });
        assert!(pinned_fut
            .as_mut()
            .poll(&mut Context::from_waker(&waker(base_waker.clone())))
            .is_pending());
        assert_eq!(Arc::strong_count(&base_waker), 2);
        tx.unbounded_send(()).unwrap();
        assert!(base_waker.waked.load(Ordering::SeqCst));
        assert!(pinned_fut
            .poll(&mut Context::from_waker(&waker(base_waker.clone())))
            .is_ready());
    }
}
