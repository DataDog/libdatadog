// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

mod mem_handle;
pub use mem_handle::*;
mod platform_handle;
pub use platform_handle::*;

#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;

mod channel;

mod message;
