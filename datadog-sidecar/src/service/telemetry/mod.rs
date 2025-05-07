// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::service::telemetry::enqueued_telemetry_data::EnqueuedTelemetryData;
pub use app_instance::AppInstance;
use futures::future::Shared;
use manual_future::ManualFuture;

mod app_instance;
pub mod enqueued_telemetry_data;
pub mod enqueued_telemetry_stats;
mod in_proc_receiver;

pub(crate) use in_proc_receiver::telemetry_action_receiver_task;
pub use in_proc_receiver::{get_telemetry_action_sender, InternalTelemetryActions};

#[allow(clippy::large_enum_variant)]
#[derive(Default)]
pub(crate) enum AppOrQueue {
    #[default]
    Inactive,
    App(Shared<ManualFuture<(String, String)>>),
    Queue(EnqueuedTelemetryData),
}
