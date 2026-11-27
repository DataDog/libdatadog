// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc as Owner, Mutex as Cell, PoisonError};
#[cfg(target_arch = "wasm32")]
use std::{cell::RefCell as Cell, rc::Rc as Owner};

#[derive(Debug, Default)]
pub struct Shared<T>(Owner<Cell<T>>);

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Shared<T> {
    pub fn read<R>(&self, operation: impl FnOnce(&T) -> R) -> R {
        #[cfg(target_arch = "wasm32")]
        let state = self.0.borrow();
        #[cfg(not(target_arch = "wasm32"))]
        let state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        operation(&state)
    }

    pub fn write<R>(&self, operation: impl FnOnce(&mut T) -> R) -> R {
        #[cfg(target_arch = "wasm32")]
        let mut state = self.0.borrow_mut();
        #[cfg(not(target_arch = "wasm32"))]
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        operation(&mut state)
    }
}
