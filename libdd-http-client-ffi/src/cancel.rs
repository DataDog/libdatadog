// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::catch_panic;
use tokio_util::sync::CancellationToken;

/// Lets another thread abort a blocked [`crate::ddog_http_client_send`].
///
/// The token must stay alive until the send has returned and no thread can still cancel it.
#[derive(Debug)]
pub struct HttpCancellationToken {
    token: CancellationToken,
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
                let will_cancel = !token.token.is_cancelled();
                token.token.cancel();
                will_cancel
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
    fn dropping_a_null_token_is_a_no_op() {
        unsafe { ddog_http_cancel_token_drop(None) };
    }
}
