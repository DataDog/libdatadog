// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::catch_panic;
use tokio_util::sync::CancellationToken;

/// A token that lets another thread abort a [`crate::ddog_http_client_send`] that is blocked.
///
/// Typical use, mirroring how a runtime releases its global lock around a blocking call:
///
/// ```c
/// ddog_HttpCancellationToken *token = ddog_http_cancel_token_new();
///
/// // Thread 1 (blocked in the call):
/// ddog_http_client_send(client, &request, token, &response);
///
/// // Thread 2 (e.g. an interrupt callback on shutdown):
/// ddog_http_cancel_token_cancel(token);
///
/// // Only once the send has returned and no thread can call cancel on the token any more:
/// ddog_http_cancel_token_drop(token);
/// ```
///
/// Both threads use the same token, so the caller must keep it alive until the send has returned
/// and no thread can still call [`ddog_http_cancel_token_cancel`] on it. Dropping it earlier is a
/// use after free.
///
/// This is deliberately a separate type from the profiler's `ddog_CancellationToken`: this crate
/// does not depend on the profiling FFI, and identical symbol names would collide in the
/// aggregated library.
#[derive(Debug)]
pub struct HttpCancellationToken {
    token: CancellationToken,
}

/// Creates a new, not yet cancelled, token. Returns `NULL` only if a panic was caught.
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

/// Cancels `token`. A blocked send using it returns [`crate::HttpClientErrorCode::Cancelled`], and
/// so does any later send. Cancellation is a terminal state.
///
/// Returns `true` if this call performed the cancellation, `false` if the token was already
/// cancelled.
///
/// # Safety
///
/// `token` must be a pointer returned by this library that has not been dropped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_cancel_token_cancel(
    token: *const HttpCancellationToken,
) -> bool {
    catch_panic!(
        {
            // SAFETY: guaranteed by the caller, see the `# Safety` section.
            match unsafe { token.as_ref() } {
                Some(token) => {
                    let will_cancel = !token.token.is_cancelled();
                    token.token.cancel();
                    will_cancel
                }
                None => false,
            }
        },
        false
    )
}

/// Drops a token. Passing `NULL` is a no-op. Dropping a token does not cancel it.
///
/// # Safety
///
/// `token` must be a pointer returned by this library that has not been dropped.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_http_cancel_token_drop(token: Option<Box<HttpCancellationToken>>) {
    catch_panic!(drop(token), ())
}
