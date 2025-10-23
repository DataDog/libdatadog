// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod channel;
pub use channel::*;

mod platform_handle;
pub use platform_handle::*;

mod message;
pub use message::*;

mod mem_handle;
pub(crate) use mem_handle::*;

mod named_pipe;
pub use named_pipe::*;
