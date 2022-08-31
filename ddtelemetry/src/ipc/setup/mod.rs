// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::{
    io,
    os::unix::net::{UnixListener, UnixStream},
};

/// Implementations of this interface must provide behavior repeatable across processes with the same version
/// of library.
/// Allowing all instances of the same version of the library to establish a shared connection
pub trait Liaison {
    fn connect_to_server(&self) -> io::Result<UnixStream>;
    fn attempt_listen(&self) -> io::Result<Option<UnixListener>>;
}

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub use unix::*;
