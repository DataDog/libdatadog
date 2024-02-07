// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
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
