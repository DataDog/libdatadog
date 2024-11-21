// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context;

pub(crate) trait ToInner {
    type Inner;
    unsafe fn to_inner_mut(&mut self) -> anyhow::Result<&mut Self::Inner>;
}

impl<T> ToInner for *mut T
where
    T: ToInner,
{
    type Inner = T::Inner;
    unsafe fn to_inner_mut(&mut self) -> anyhow::Result<&mut Self::Inner> {
        let inner = self.as_mut().context("Null pointer")?;
        inner.to_inner_mut()
    }
}
