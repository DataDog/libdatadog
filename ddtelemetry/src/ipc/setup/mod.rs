// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use std::io;

pub trait Liaison<Conn, Listener>: Sized {
    fn connect_to_server(&self) -> io::Result<Conn>;
    fn attempt_listen(&self) -> io::Result<Option<Listener>>;
}

pub trait ServerLiason: Sized {}

#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub use unix::*;
