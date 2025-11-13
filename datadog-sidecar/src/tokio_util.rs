// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use futures::future::Shared;
use std::future::Future;

#[macro_export]
macro_rules! spawn_map_err {
    ($fut:expr, $err:expr) => {
        tokio::spawn(async move {
            if let Err(e) = tokio::spawn($fut).await {
                ($err)(e);
            }
        })
    };
}

pub fn run_or_spawn_shared<F: Future + Send + 'static>(
    fut: Shared<F>,
    f: impl FnOnce(&F::Output) + Send + 'static,
) where
    F::Output: Clone + Sync + Send,
{
    if let Some(out) = fut.peek() {
        f(out);
    } else {
        tokio::spawn(async move { f(&fut.await) });
    }
}
