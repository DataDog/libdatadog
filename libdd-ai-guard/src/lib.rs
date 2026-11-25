// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod handler;
pub mod register;
pub mod transport;

pub use handler::{DEBUG_PING_URI, ReceivedPayload, handle_dispatch, run_dispatch_loop};
pub use register::{HostApplication, RegisterRequest, register_connection};
pub use transport::{AiGuardX509, AiGuardX509Config, AiGuardX509Error};
