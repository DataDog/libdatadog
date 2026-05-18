// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod platform_handle;
pub use platform_handle::*;

mod mem_handle;
pub(crate) use mem_handle::*;

mod named_pipe;
pub use named_pipe::*;

pub mod sockets;
pub use sockets::*;

mod handles;
pub use handles::*;
