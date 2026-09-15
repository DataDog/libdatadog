// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub struct CancellationToken {
    pub(crate) inner: tokio_util::sync::CancellationToken,
}

impl CancellationToken {
    /// Creates a new cancellation token.
    pub fn create() -> Box<CancellationToken> {
        Box::new(CancellationToken {
            inner: tokio_util::sync::CancellationToken::new(),
        })
    }

    /// Clones the cancellation token.
    ///
    /// A cloned token is connected to the original token - either can be used
    /// to cancel or check cancellation status. The useful part is that they have
    /// independent lifetimes and can be dropped separately.
    ///
    /// This is useful for multi-threaded scenarios where one thread performs the
    /// send operation while another thread can cancel it.
    #[allow(clippy::should_implement_trait)]
    pub fn clone(&self) -> Box<CancellationToken> {
        Box::new(CancellationToken {
            inner: self.inner.clone(),
        })
    }

    /// Cancels the token.
    ///
    /// Note that cancellation is a terminal state; calling cancel multiple times
    /// has no additional effect.
    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// Returns true if the token has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }
}
