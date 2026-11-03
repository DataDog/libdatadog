// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use futures::channel::oneshot;
use futures::lock::Mutex;
use libdd_capabilities::{HttpClientCapability, HttpError, SleepCapability};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Event {
    Http {
        id: i32,
        method: String,
        url: String,
        headers: Vec<(String, String)>,
        body: String,
    },
    Sleep {
        id: i32,
        milliseconds: u64,
    },
    Cancel {
        id: i32,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    status: Option<u16>,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    body: String,
    error: Option<String>,
}

#[derive(Default, Debug)]
struct Bridge {
    next: i32,
    events: Vec<Event>,
    pending: HashMap<i32, oneshot::Sender<Reply>>,
}

// Events contain no host objects. Both engines drive these queues after guest calls return.
#[derive(Clone, Default, Debug)]
pub struct Host(Arc<Mutex<Bridge>>);

struct Wait {
    host: Host,
    id: i32,
    receiver: oneshot::Receiver<Reply>,
}

impl Drop for Wait {
    fn drop(&mut self) {
        // No bridge guard survives a guest yield, and there is no host callback re-entry.
        if let Some(mut bridge) = self.host.0.try_lock()
            && bridge.pending.remove(&self.id).is_some()
        {
            bridge.events.push(Event::Cancel { id: self.id });
        }
    }
}

impl Host {
    fn wait(&self, event: impl FnOnce(i32) -> Event) -> Result<Wait> {
        let mut bridge = self.0.try_lock().context("host bridge is busy")?;
        bridge.next = bridge
            .next
            .checked_add(1)
            .context("operation IDs exhausted")?;
        let id = bridge.next;
        let (sender, receiver) = oneshot::channel();
        bridge.pending.insert(id, sender);
        bridge.events.push(event(id));
        Ok(Wait {
            host: self.clone(),
            id,
            receiver,
        })
    }

    pub fn events(&self) -> Result<Vec<Event>> {
        Ok(std::mem::take(
            &mut self.0.try_lock().context("host bridge is busy")?.events,
        ))
    }

    pub fn complete(&self, id: i32, reply: Reply) -> Result<()> {
        let sender = self
            .0
            .try_lock()
            .context("host bridge is busy")?
            .pending
            .remove(&id)
            .context("stale or completed operation")?;
        ensure!(sender.send(reply).is_ok(), "operation no longer active");
        Ok(())
    }
}

impl HttpClientCapability for Host {
    fn new_client() -> Self {
        Self::default()
    }
    fn new_periodic() -> Self {
        Self::default()
    }

    async fn request(
        &self,
        request: http::Request<Bytes>,
    ) -> Result<http::Response<Bytes>, HttpError> {
        let operation = || -> Result<_> {
            let (parts, body) = request.into_parts();
            let headers = parts
                .headers
                .iter()
                .map(|(name, value)| Ok((name.to_string(), value.to_str()?.to_string())))
                .collect::<Result<_>>()?;
            self.wait(|id| Event::Http {
                id,
                method: parts.method.to_string(),
                url: parts.uri.to_string(),
                headers,
                body: STANDARD.encode(body),
            })
        };
        let mut wait = operation().map_err(HttpError::InvalidRequest)?;
        let reply = (&mut wait.receiver)
            .await
            .map_err(|error| HttpError::Network(error.into()))?;
        if let Some(error) = reply.error {
            return Err(HttpError::Network(anyhow::anyhow!(error)));
        }
        let mut builder = http::Response::builder().status(
            reply
                .status
                .ok_or_else(|| HttpError::Other(anyhow::anyhow!("missing response status")))?,
        );
        for (name, value) in reply.headers {
            builder = builder.header(name, value);
        }
        builder
            .body(Bytes::from(
                STANDARD
                    .decode(reply.body)
                    .map_err(|error| HttpError::ResponseBody(error.into()))?,
            ))
            .map_err(|error| HttpError::Other(error.into()))
    }
}

impl SleepCapability for Host {
    fn new() -> Self {
        Self::default()
    }

    async fn sleep(&self, duration: Duration) {
        // Duration originates from the validated u64 timeout option, so milliseconds fit u64.
        let milliseconds = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        if let Ok(mut wait) = self.wait(|id| Event::Sleep { id, milliseconds }) {
            let _completion = (&mut wait.receiver).await;
        }
    }
}
