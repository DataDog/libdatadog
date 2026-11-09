// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Requests exceeding the message size limit of a connection travel in shared memory.
//!
//! The kernel limit may be well below the configured [`max_message_size`]: Linux caps socket
//! buffers at twice `net.core.wmem_max`, which containers cannot raise. Instead of the request,
//! such a message carries the `_Oversized` variant generated for every request enum by
//! `#[service]`. Its shared memory handle is transferred before the handles of the request.

use crate::codec::DecodeError;
use crate::handles::{FdSink, TransferHandles};
use crate::max_message_size;
use crate::platform::{FileBackedHandle, ShmHandle};
use serde::Serialize;
use std::io;

#[cfg(unix)]
pub use std::os::unix::io::OwnedFd;
#[cfg(unix)]
use std::os::unix::io::RawFd;
#[cfg(windows)]
pub use std::os::windows::io::OwnedHandle as OwnedFd;
#[cfg(windows)]
use std::os::windows::io::RawHandle as RawFd;

/// A request enum able to carry an encoded request of its own type in shared memory.
pub trait OversizedEnvelope {
    fn oversized(shm: ShmHandle, len: usize) -> Self;
}

/// A request encoded for sending, along with the handles to transfer.
pub struct EncodedRequest {
    pub data: Vec<u8>,
    pub fds: Vec<RawFd>,
    /// Owns the shared memory `fds` refers to, until sent.
    _shm: Option<ShmHandle>,
}

/// Bytes appended to a message for its handles.
fn suffix_size(fds: usize) -> usize {
    if cfg!(windows) { 4 + 8 * fds } else { 0 }
}

fn collect_fds<T: TransferHandles>(requests: &[&T]) -> Vec<RawFd> {
    let mut sink = FdSink::new();
    for request in requests {
        request.copy_handles(&mut sink).ok();
    }
    sink.into_fds()
}

/// Encodes `req`, moving it to shared memory if it exceeds `max_send_size`.
///
/// Fails with [`io::ErrorKind::FileTooLarge`] beyond [`max_message_size`], to keep in-flight
/// memory bounded by the configured limit.
pub fn encode_request<T>(req: &T, max_send_size: usize) -> io::Result<EncodedRequest>
where
    T: Serialize + TransferHandles + OversizedEnvelope,
{
    let fds = collect_fds(&[req]);
    let data = crate::codec::encode_with_reserve(req, suffix_size(fds.len()));
    if data.len() <= max_send_size {
        return Ok(EncodedRequest {
            data,
            fds,
            _shm: None,
        });
    }
    let max = max_message_size();
    if data.len() > max {
        tracing::warn!(len = data.len(), max, "IPC message too large");
        return Err(io::Error::from(io::ErrorKind::FileTooLarge));
    }

    let mut mapped = ShmHandle::new(data.len())
        .and_then(|shm| Ok(shm.map()?))
        .map_err(io::Error::other)?;
    let Some(target) = mapped.as_slice_mut().get_mut(..data.len()) else {
        return Err(io::Error::other("shared memory smaller than requested"));
    };
    target.copy_from_slice(&data);
    let shm: ShmHandle = mapped.into();
    let envelope = T::oversized(shm.clone(), data.len());
    let fds = collect_fds(&[&envelope, req]);
    Ok(EncodedRequest {
        data: crate::codec::encode_with_reserve(&envelope, suffix_size(fds.len())),
        fds,
        _shm: Some(shm),
    })
}

/// Decodes the request carried in `shm`, whose handle was already received.
pub fn decode<T: serde::de::DeserializeOwned>(
    shm: ShmHandle,
    len: usize,
) -> Result<T, DecodeError> {
    if len > max_message_size() {
        return Err(DecodeError::Io(io::Error::from(
            io::ErrorKind::FileTooLarge,
        )));
    }
    // Receiving the handle bounded its size by the backing file, which the peer controls.
    let mapped = shm.map().map_err(DecodeError::Io)?;
    let payload = mapped
        .as_slice()
        .get(..len)
        .ok_or_else(|| DecodeError::Io(io::Error::from(io::ErrorKind::UnexpectedEof)))?;
    crate::codec::decode(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::example_interface::ExampleInterfaceRequest;

    #[test]
    #[cfg_attr(miri, ignore)]
    fn small_requests_stay_inline() {
        let req = ExampleInterfaceRequest::EchoLen {
            payload: vec![1; 10],
        };
        let encoded = encode_request(&req, 1024).unwrap();
        assert!(encoded._shm.is_none() && encoded.fds.is_empty());
        assert!(matches!(
            crate::codec::decode(&encoded.data).unwrap(),
            ExampleInterfaceRequest::EchoLen { payload } if payload == [1; 10]
        ));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn large_requests_carry_shared_memory() {
        let req = ExampleInterfaceRequest::EchoLen {
            payload: vec![1; 4096],
        };
        let encoded = encode_request(&req, 1024).unwrap();
        assert!(encoded._shm.is_some());
        assert_eq!(encoded.fds.len(), 1);
        assert!(encoded.data.len() < 1024);
        assert!(matches!(
            crate::codec::decode(&encoded.data).unwrap(),
            ExampleInterfaceRequest::_Oversized { len, .. } if len > 4096
        ));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn requests_beyond_the_configured_limit_are_rejected() {
        let req = ExampleInterfaceRequest::EchoLen {
            payload: vec![1; max_message_size()],
        };
        let err = encode_request(&req, 1024).err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::FileTooLarge);
    }
}
