// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//! Future wrapper that holds the task waker weakly.
//!
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

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), deny(clippy::panic))]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![cfg_attr(not(test), deny(clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::todo))]
#![cfg_attr(not(test), deny(clippy::unimplemented))]

extern crate alloc;

use alloc::sync::Arc;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use futures_util::task::{ArcWake, AtomicWaker, waker_ref};

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

/// Future wrapper that passes a weak waker to the inner future.
///
/// See the [crate-level documentation](crate) for details.
pub struct WeakWakerFuture<F: Future> {
    fut: F,
    weak_waker: WeakWaker,
}

impl<F: Future> WeakWakerFuture<F> {
    /// Wrap a future so that the waker passed to it is held only weakly.
    ///
    /// See the [crate-level documentation](crate) for details.
    #[must_use = "futures do nothing unless you `.await` or poll them"]
    pub fn new(fut: F) -> WeakWakerFuture<F> {
        WeakWakerFuture {
            fut,
            weak_waker: WeakWaker {
                inner: Arc::new(WeakWakerInner {
                    waker: AtomicWaker::new(),
                }),
            },
        }
    }
}

impl<F: Future> Future for WeakWakerFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: `WeakWakerFuture` is never moved out of the pin. Neither `weak_waker` nor `fut`
        // are moved out of `self` below: `fut` is only re-pinned in place, and `weak_waker` is
        // only borrowed.
        let m = unsafe { self.get_unchecked_mut() };

        // Update the stored waker in-place via
        // AtomicWaker::register — no heap allocation.
        m.weak_waker.inner.waker.register(cx.waker());

        // SAFETY: structural pinning for `fut`. The shared borrow of `m.weak_waker`
        // and the mutable borrow of `m.fut` are on disjoint fields; NLL allows this.
        unsafe {
            Pin::new_unchecked(&mut m.fut)
                .poll(&mut Context::from_waker(&waker_ref(&m.weak_waker.inner)))
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::future::Future;
    use core::pin::{Pin, pin};
    use core::sync::atomic::{AtomicBool, Ordering};
    use core::task::Context;

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
    fn test_mpsc_queue_weak_waker_drop_correctly() {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();

        let mut fut = WeakWakerFuture::new(futures::StreamExt::next(&mut rx));
        let pinned_fut = Pin::new(&mut fut);
        let base_waker = Arc::new(TestWaker {
            waked: AtomicBool::new(false),
        });
        assert!(
            pinned_fut
                .poll(&mut Context::from_waker(&waker(base_waker.clone())))
                .is_pending()
        );
        assert_eq!(Arc::strong_count(&base_waker), 2);
        drop(fut);
        assert!(!base_waker.waked.load(Ordering::SeqCst));
        assert_eq!(Arc::strong_count(&base_waker), 1);
        tx.unbounded_send(()).unwrap();
    }

    #[test]
    fn test_mpsc_queue_weak_waker_smoke() {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();

        let mut pinned_fut = pin!(WeakWakerFuture::new(futures::StreamExt::next(&mut rx)));
        let base_waker = Arc::new(TestWaker {
            waked: AtomicBool::new(false),
        });
        assert!(
            pinned_fut
                .as_mut()
                .poll(&mut Context::from_waker(&waker(base_waker.clone())))
                .is_pending()
        );
        assert_eq!(Arc::strong_count(&base_waker), 2);
        tx.unbounded_send(()).unwrap();
        assert!(base_waker.waked.load(Ordering::SeqCst));
        assert!(
            pinned_fut
                .poll(&mut Context::from_waker(&waker(base_waker.clone())))
                .is_ready()
        );
    }

    #[test]
    fn test_weak_waker_wakes_latest_registered_waker() {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();

        let mut pinned_fut = pin!(WeakWakerFuture::new(futures::StreamExt::next(&mut rx)));
        let first = Arc::new(TestWaker {
            waked: AtomicBool::new(false),
        });
        let second = Arc::new(TestWaker {
            waked: AtomicBool::new(false),
        });
        assert!(
            pinned_fut
                .as_mut()
                .poll(&mut Context::from_waker(&waker(first.clone())))
                .is_pending()
        );
        assert!(
            pinned_fut
                .as_mut()
                .poll(&mut Context::from_waker(&waker(second.clone())))
                .is_pending()
        );
        // The first waker is replaced, so the future no longer holds it.
        assert_eq!(Arc::strong_count(&first), 1);
        tx.unbounded_send(()).unwrap();
        assert!(!first.waked.load(Ordering::SeqCst));
        assert!(second.waked.load(Ordering::SeqCst));
    }
}
