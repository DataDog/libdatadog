// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Re-export of [`libdd_weak_waker::WeakWakerFuture`].

pub(crate) use libdd_weak_waker::WeakWakerFuture;

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::{Pin, pin};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
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
}
