// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::catch_panic;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_util::sync::CancellationToken;

/// Lets another thread abort a blocked [`crate::ddog_http_client_send`].
///
/// The token must stay alive until the send has returned and no thread can still cancel it.
#[derive(Debug)]
pub struct HttpCancellationToken {
    token: CancellationToken,
    /// Set by the first call to cancel; the swap makes the claim atomic.
    claimed: AtomicBool,
}

impl HttpCancellationToken {
    pub(crate) fn cancellation_token(&self) -> &CancellationToken {
        &self.token
    }
}

/// Creates a token. Returns `NULL` only if a panic was caught.
#[unsafe(no_mangle)]
#[must_use]
pub extern "C" fn ddog_http_cancel_token_new() -> Option<Box<HttpCancellationToken>> {
    catch_panic!(
        Some(Box::new(HttpCancellationToken {
            token: CancellationToken::new(),
            claimed: AtomicBool::new(false),
        })),
        None
    )
}

/// Cancels `token`: a blocked or later send returns `Cancelled`. Returns `true` if this call
/// cancelled it, `false` if it already was.
///
/// # Safety
///
/// `token` must be a live token pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_cancel_token_cancel(
    token: *const HttpCancellationToken,
) -> bool {
    catch_panic!(
        match unsafe { token.as_ref() } {
            Some(token) => {
                let first = !token.claimed.swap(true, Ordering::AcqRel);
                token.token.cancel();
                first
            }
            None => false,
        },
        false
    )
}

/// Drops a token. Passing `NULL` is a no-op.
///
/// # Safety
///
/// `token` must be `NULL` or a live token pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_cancel_token_drop(token: Option<Box<HttpCancellationToken>>) {
    catch_panic!(drop(token), ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelling_is_idempotent_and_reports_only_the_first_call() {
        let token = ddog_http_cancel_token_new().expect("a token");

        assert!(unsafe { ddog_http_cancel_token_cancel(&*token) });
        assert!(!unsafe { ddog_http_cancel_token_cancel(&*token) });
        assert!(!unsafe { ddog_http_cancel_token_cancel(&*token) });
    }

    #[test]
    fn exactly_one_concurrent_caller_cancels_the_token() {
        const CALLERS: usize = 16;
        for _ in 0..50 {
            let token = ddog_http_cancel_token_new().expect("a token");
            let barrier = std::sync::Barrier::new(CALLERS);
            let wins: usize = std::thread::scope(|scope| {
                let handles: Vec<_> = (0..CALLERS)
                    .map(|_| {
                        scope.spawn(|| {
                            barrier.wait();
                            unsafe { ddog_http_cancel_token_cancel(&*token) }
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| usize::from(h.join().expect("thread")))
                    .sum()
            });
            assert_eq!(wins, 1);
            assert!(token.cancellation_token().is_cancelled());
        }
    }

    #[test]
    fn dropping_a_null_token_is_a_no_op() {
        unsafe { ddog_http_cancel_token_drop(None) };
    }
}
