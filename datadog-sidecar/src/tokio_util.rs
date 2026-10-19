// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

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
