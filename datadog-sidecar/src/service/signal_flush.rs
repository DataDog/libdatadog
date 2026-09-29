// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Prepared flushes sent on the normal connection, with a private completion pipe.

use super::{
    SidecarFlushOptions, blocking::SidecarTransport, sidecar_interface::SidecarInterfaceRequest,
};
use libdd_ipc::{platform::PlatformHandle, signal_safe::PreparedRequest};
use std::io;

/// One pre-encoded flush. The duplicate fd preserves the template connection until drop; it
/// shares packet ordering with normal sends and never reads the normal client's replies.
/// Construction and destruction require ordinary thread context.
pub struct SignalFlush {
    request: PreparedRequest,
}

impl SignalFlush {
    /// Prepare against this exact transport. No RPC, listener lookup, or sidecar task is needed.
    /// Refresh after reconnect and drop before shutting down the normal sidecar connection.
    pub fn prepare(
        template: &mut SidecarTransport,
        options: SidecarFlushOptions,
    ) -> io::Result<Self> {
        let sender = template
            .inner
            .get_mut()
            .map_err(|_| io::Error::other("poisoned sidecar transport"))?;
        let (receiver, completion) = io::pipe()?;
        let completion = PlatformHandle::from(completion);
        let message = SidecarInterfaceRequest::FlushSignal {
            options,
            completion: completion.clone(),
        };
        let bytes = libdd_ipc::codec::encode(&message).into_boxed_slice();
        drop(message);
        let request = PreparedRequest::new(
            &sender.channel.0.conn,
            bytes,
            receiver,
            completion.into_owned_handle()?,
        )?;
        Ok(Self { request })
    }

    /// Execute one bounded flush without allocation, locks, TLS access, or process termination.
    ///
    /// # Safety
    /// The caller must guarantee exclusive one-shot use, keep this object alive until return,
    /// and block all worker signals. Do not use an inherited object after fork.
    #[inline(always)]
    pub unsafe fn run(&self) -> i32 {
        unsafe { self.request.exchange() }
    }
}
