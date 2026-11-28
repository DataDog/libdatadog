// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#![cfg(all(target_os = "wasi", target_pointer_width = "32"))]

use core::cell::RefCell;
use std::collections::HashMap;

use libdd_config_core::WireResolver;

const DESCRIPTOR_LENGTH: usize = 8;
const SUCCESS: u32 = 0;
const BAD_BUFFER: u32 = 1;
const BAD_RESPONSE: u32 = 2;

#[derive(Default)]
struct Reactor {
    resolver: WireResolver,
    buffers: HashMap<u32, Box<[u8]>>,
}

thread_local! {
    static REACTOR: RefCell<Reactor> = RefCell::default();
}

// allow(ffi-panic-boundary): A panic in this Wasm guest traps to the host.
#[unsafe(no_mangle)]
pub extern "C" fn config_v1_alloc(length: u32) -> u32 {
    if length == 0 {
        return 0;
    }
    REACTOR.with(|reactor| {
        let Ok(mut reactor) = reactor.try_borrow_mut() else {
            return 0;
        };
        let mut bytes = Vec::new();
        if bytes.try_reserve_exact(length as usize).is_err() {
            return 0;
        }
        bytes.resize(length as usize, 0);
        reactor.store(bytes)
    })
}

// allow(ffi-panic-boundary): A panic in this Wasm guest traps to the host.
#[unsafe(no_mangle)]
pub extern "C" fn config_v1_resolve(input: u32, length: u32, descriptor: u32) -> u32 {
    REACTOR.with(|reactor| {
        let Ok(mut reactor) = reactor.try_borrow_mut() else {
            return BAD_BUFFER;
        };
        reactor.resolve(input, length as usize, descriptor)
    })
}

// allow(ffi-panic-boundary): A panic in this Wasm guest traps to the host.
#[unsafe(no_mangle)]
pub extern "C" fn config_v1_free(pointer: u32) -> u32 {
    REACTOR.with(|reactor| {
        let Ok(mut reactor) = reactor.try_borrow_mut() else {
            return BAD_BUFFER;
        };
        if reactor.buffers.remove(&pointer).is_some() {
            SUCCESS
        } else {
            BAD_BUFFER
        }
    })
}

impl Reactor {
    fn store(&mut self, bytes: Vec<u8>) -> u32 {
        let mut bytes = bytes.into_boxed_slice();
        let pointer = bytes.as_mut_ptr() as u32;
        self.buffers.insert(pointer, bytes);
        pointer
    }

    fn resolve(&mut self, input: u32, length: usize, descriptor: u32) -> u32 {
        let Some(descriptor_bytes) = self.buffers.get(&descriptor) else {
            return BAD_BUFFER;
        };
        if descriptor_bytes.len() != DESCRIPTOR_LENGTH || input == descriptor {
            return BAD_BUFFER;
        }

        let Some(request) = self
            .buffers
            .get(&input)
            .and_then(|bytes| bytes.get(..length))
        else {
            return BAD_BUFFER;
        };
        let Ok(response) = self.resolver.request(request) else {
            return BAD_RESPONSE;
        };

        #[allow(
            clippy::cast_possible_truncation,
            reason = "The reactor uses 32-bit linear memory"
        )]
        let length = response.len() as u32;
        let pointer = self.store(response);
        let Some(descriptor_bytes) = self.buffers.get_mut(&descriptor) else {
            return BAD_BUFFER;
        };
        descriptor_bytes[..4].copy_from_slice(&pointer.to_le_bytes());
        descriptor_bytes[4..].copy_from_slice(&length.to_le_bytes());
        SUCCESS
    }
}
