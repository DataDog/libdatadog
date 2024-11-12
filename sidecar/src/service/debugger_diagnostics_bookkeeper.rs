use datadog_live_debugger::debugger_defs::{
    DebuggerData, DebuggerPayload, Diagnostics, ProbeStatus,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::select;
use tokio_util::sync::CancellationToken;

pub struct DebuggerDiagnosticsBookkeeper {
    active_by_runtime_id: Arc<Mutex<HashMap<String, DebuggerActiveData>>>,
    cancel: CancellationToken,
}

struct LastProbeStatus {
    status: ProbeStatus,
    last_update: Instant,
}

#[derive(Default)]
struct DebuggerActiveData {
    pub active_probes: HashMap<String, LastProbeStatus>,
}

const MAX_TIME_BEFORE_FALLBACK: Duration = Duration::from_secs(300);
const MAX_TIME_BEFORE_REMOVAL: Duration = Duration::from_secs(600);

impl DebuggerDiagnosticsBookkeeper {
    pub fn start() -> DebuggerDiagnosticsBookkeeper {
        let buffer = DebuggerDiagnosticsBookkeeper {
            active_by_runtime_id: Default::default(),
            cancel: CancellationToken::new(),
        };
        let active = buffer.active_by_runtime_id.clone();
        let cancel = buffer.cancel.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(MAX_TIME_BEFORE_REMOVAL / 2);
            loop {
                select! {
                    _ = interval.tick() => {
                        active.lock().unwrap().retain(|_, active| {
                            active.active_probes.retain(|_, status| {
                                status.last_update.elapsed() < MAX_TIME_BEFORE_REMOVAL
                            });
                            active.active_probes.len() != 0
                        });
                    },
                    _ = cancel.cancelled() => {
                        break;
                    },
                }
            }
        });
        buffer
    }

    pub fn add_payload(&self, payload: &DebuggerPayload) -> bool {
        if let DebuggerData::Diagnostics(diagnostics) = &payload.debugger {
            let mut send = true;

            fn insert_probe(active_data: &mut DebuggerActiveData, diagnostics: &Diagnostics) {
                active_data.active_probes.insert(
                    diagnostics.probe_id.to_string(),
                    LastProbeStatus {
                        status: diagnostics.status,
                        last_update: Instant::now(),
                    },
                );
            }

            let mut buffers = self.active_by_runtime_id.lock().unwrap();
            if let Some(buffer) = buffers.get_mut(diagnostics.runtime_id.as_ref()) {
                if let Some(status) = buffer
                    .active_probes
                    .get_mut(diagnostics.runtime_id.as_ref())
                {
                    if !matches!(diagnostics.status, ProbeStatus::Received) {
                        if matches!(status.status, ProbeStatus::Received)
                            || (!matches!(diagnostics.status, ProbeStatus::Installed)
                                && matches!(status.status, ProbeStatus::Installed))
                        {
                            if status.last_update.elapsed() < MAX_TIME_BEFORE_FALLBACK {
                                send = false;
                            }
                        }
                    }
                    if send {
                        status.last_update = Instant::now();
                        if status.status != diagnostics.status {
                            status.status = diagnostics.status;
                        } else {
                            send = false;
                        }
                    }
                } else {
                    insert_probe(buffer, &diagnostics);
                }
            } else {
                buffers.insert(diagnostics.runtime_id.to_string(), {
                    let mut data = DebuggerActiveData::default();
                    insert_probe(&mut data, &diagnostics);
                    data
                });
            }

            send
        } else {
            unreachable!("This is only for diagnostics");
        }
    }

    pub fn stats(&self) -> DebuggerDiagnosticsBookkeeperStats {
        let buffers = self.active_by_runtime_id.lock().unwrap();
        DebuggerDiagnosticsBookkeeperStats {
            runtime_ids: buffers.len() as u32,
            total_probes: buffers
                .iter()
                .map(|(_, active)| active.active_probes.len() as u32)
                .sum(),
        }
    }
}

impl Default for DebuggerDiagnosticsBookkeeper {
    fn default() -> Self {
        Self::start()
    }
}

impl Drop for DebuggerDiagnosticsBookkeeper {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Serialize, Deserialize)]
pub struct DebuggerDiagnosticsBookkeeperStats {
    runtime_ids: u32,
    total_probes: u32,
}
