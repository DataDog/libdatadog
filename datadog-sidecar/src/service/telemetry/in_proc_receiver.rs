// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Contains the background task that receives telemetry actions via an internal queue
// and forwards them to the appropriate telemetry worker.

use crate::service::runtime_info::ActiveApplication;
use crate::service::telemetry::AppOrQueue;
use crate::service::{InstanceId, QueueId, SidecarAction, SidecarServer};
use anyhow::{anyhow, Result};
use ddcommon::MutexExt;
use ddtelemetry::worker::TelemetryActions;
use std::collections::hash_map::Entry;
use std::sync::OnceLock;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::enqueued_telemetry_data::EnqueuedTelemetryData;

static TELEMETRY_ACTION_SENDER: OnceLock<mpsc::Sender<InternalTelemetryActions>> = OnceLock::new();

#[derive(Debug)]
pub struct InternalTelemetryActions {
    pub instance_id: InstanceId,
    pub queue_id: QueueId,
    pub actions: Vec<TelemetryActions>,
}

pub fn get_telemetry_action_sender() -> Result<mpsc::Sender<InternalTelemetryActions>> {
    TELEMETRY_ACTION_SENDER
        .get()
        .cloned()
        .ok_or_else(|| anyhow!("Telemetry action sender not initialized"))
}

pub(crate) async fn telemetry_action_receiver_task(sidecar: SidecarServer) {
    info!("Starting telemetry action receiver task...");

    // create mpsc pair and set TELEMETRY_ACTION_SENDER
    let (tx, mut rx) = mpsc::channel(1000);
    if TELEMETRY_ACTION_SENDER.set(tx).is_err() {
        warn!("Failed to set telemetry action sender");
        return;
    }

    while let Some(msg) = rx.recv().await {
        if let Err(e) =
            process_telemetry_action(&sidecar, &msg.instance_id, &msg.queue_id, msg.actions).await
        {
            warn!(
                "Could not process telemetry action for target {:?}/{:?}: {}. Action dropped.",
                msg.instance_id, msg.queue_id, e
            );
        }
    }
    info!("Telemetry action receiver task shutting down.");
}

async fn process_telemetry_action(
    sidecar: &SidecarServer,
    instance_id: &InstanceId,
    queue_id: &QueueId,
    actions: Vec<TelemetryActions>,
) -> Result<()> {
    tracing::debug!(
        "Processing telemetry action for target {:?}/{:?}: {:?}",
        instance_id,
        queue_id,
        actions
    );

    let session_info = sidecar.get_session(&instance_id.session_id);
    let runtime_info = session_info.get_runtime(&instance_id.runtime_id);
    let mut applications = runtime_info.lock_applications();

    match applications.entry(*queue_id) {
        Entry::Occupied(mut occupied_entry) => {
            let active_app = occupied_entry.get_mut();
            match active_app.app_or_actions {
                AppOrQueue::Queue(ref mut etd) => {
                    etd.actions.extend(actions);
                    Ok(())
                }
                AppOrQueue::App(ref service_fut) => {
                    let service_fut = service_fut.clone();
                    let apps = runtime_info.apps.clone();
                    let instance_id = instance_id.clone();
                    let queue_id = *queue_id;

                    tokio::spawn(async move {
                        let service = service_fut.await;
                        let app_future = if let Some(fut) = apps.lock_or_panic().get(&service) {
                            fut.clone()
                        } else {
                            warn!("No application future found for service {:?} from target {:?}/{:?}. Actions {:?} dropped.", service, instance_id, queue_id, actions);
                            return;
                        };
                        match app_future.await {
                            Some(app_instance) => {
                                if let Err(e) = app_instance.telemetry.send_msgs(actions).await {
                                    warn!("Failed to send telemetry action to worker for {:?}/{:?}: {}", instance_id, queue_id, e);
                                }
                            }
                            None => {
                                warn!("AppInstance future resolved to None for service {:?} from target {:?}/{:?}. Actions {:?} dropped.", service, instance_id, queue_id, actions);
                            }
                        }
                    });
                    Ok(())
                }
                ref mut app_or_queue @ AppOrQueue::Inactive => {
                    *app_or_queue = AppOrQueue::Queue(EnqueuedTelemetryData::processed(
                        actions.into_iter().map(SidecarAction::Telemetry).collect(),
                    ));
                    Ok(())
                }
            }
        }
        Entry::Vacant(entry) => {
            entry.insert(ActiveApplication {
                app_or_actions: AppOrQueue::Queue(EnqueuedTelemetryData::processed(
                    actions.into_iter().map(SidecarAction::Telemetry).collect(),
                )),
                ..Default::default()
            });
            Ok(())
        }
    }
}
