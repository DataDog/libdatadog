// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

use datadog_ipc::platform::Channel;
use std::io;

/// Implementations of this interface must provide behavior repeatable across processes with the
/// same version of library.
/// Allowing all instances of the same version of the library to establish a shared connection
pub trait Liaison: Sized {
    fn connect_to_server(&self) -> io::Result<Channel>;
    fn attempt_listen(&self) -> io::Result<Option<IpcServer>>;
    fn ipc_shared() -> Self;
    fn ipc_per_process() -> Self;
    fn for_master_pid(master_pid: u32) -> Self;
}
