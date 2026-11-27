// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use futures_channel::oneshot;
use libdd_capabilities::{HttpClientCapability, HttpError, SleepCapability};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::time::Duration;

pub(crate) const MAX_BUFFER: usize = 16 * 1024 * 1024;

type HttpResult = Result<http::Response<Bytes>, HttpError>;

#[derive(Debug)]
pub enum Event {
    Http {
        id: i32,
        method: http::Method,
        url: String,
        headers: Vec<(String, String)>,
        bytes: Bytes,
    },
    Sleep {
        id: i32,
        delay_ms: u32,
    },
    Cancel {
        id: i32,
    },
}

#[derive(Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum EventDescriptor<'event> {
    Http {
        id: i32,
        method: &'event str,
        url: &'event str,
        headers: &'event [(String, String)],
        body: i32,
    },
    Sleep {
        id: i32,
        delay_ms: u32,
    },
    Cancel {
        id: i32,
    },
}

impl Event {
    pub fn describe(&self, body: i32) -> EventDescriptor<'_> {
        match self {
            Self::Http {
                id,
                method,
                url,
                headers,
                ..
            } => EventDescriptor::Http {
                id: *id,
                method: method.as_str(),
                url,
                headers,
                body,
            },
            Self::Sleep { id, delay_ms } => EventDescriptor::Sleep {
                id: *id,
                delay_ms: *delay_ms,
            },
            Self::Cancel { id } => EventDescriptor::Cancel { id: *id },
        }
    }
}

#[derive(Debug)]
enum Pending {
    Http(oneshot::Sender<HttpResult>),
    Timer(oneshot::Sender<()>),
}

#[derive(Default, Debug)]
struct Bridge {
    next_id: i32,
    events: VecDeque<Event>,
    pending: BTreeMap<i32, Pending>,
}

#[derive(Clone, Default, Debug)]
pub struct Host(Rc<RefCell<Bridge>>);

struct Operation {
    id: i32,
    host: Host,
}

impl Drop for Operation {
    fn drop(&mut self) {
        let mut bridge = self.host.0.borrow_mut();
        if bridge.pending.remove(&self.id).is_some() {
            let queued = bridge.events.iter().position(|event| match event {
                Event::Http { id, .. } | Event::Sleep { id, .. } => *id == self.id,
                Event::Cancel { .. } => false,
            });
            if let Some(index) = queued {
                bridge.events.remove(index);
            } else {
                bridge.events.push_back(Event::Cancel { id: self.id });
            }
        }
    }
}

impl Host {
    fn start(&self, reply: Pending, event: impl FnOnce(i32) -> Event) -> Result<Operation> {
        let mut bridge = self.0.borrow_mut();
        let id = bridge
            .next_id
            .checked_add(1)
            .context("operation IDs exhausted")?;
        bridge.next_id = id;
        bridge.pending.insert(id, reply);
        bridge.events.push_back(event(id));
        Ok(Operation {
            id,
            host: self.clone(),
        })
    }

    pub fn event(&self) -> Option<Event> {
        self.0.borrow_mut().events.pop_front()
    }

    pub fn restore_event(&self, event: Event) {
        self.0.borrow_mut().events.push_front(event);
    }

    pub fn expects_http(&self, operation: i32) -> bool {
        matches!(
            self.0.borrow().pending.get(&operation),
            Some(Pending::Http(_))
        )
    }

    pub fn complete_http(&self, operation: i32, reply: HttpResult) -> Result<()> {
        ensure!(self.expects_http(operation), "not a pending HTTP operation");
        let pending = self.0.borrow_mut().pending.remove(&operation);
        if let Some(Pending::Http(sender)) = pending {
            ensure!(
                sender.send(reply).is_ok(),
                "HTTP operation no longer active"
            );
        }
        Ok(())
    }

    pub fn complete_timer(&self, operation: i32) -> Result<()> {
        let mut bridge = self.0.borrow_mut();
        ensure!(
            matches!(bridge.pending.get(&operation), Some(Pending::Timer(_))),
            "not a pending timer operation"
        );
        let pending = bridge.pending.remove(&operation);
        drop(bridge);
        if let Some(Pending::Timer(sender)) = pending {
            ensure!(sender.send(()).is_ok(), "timer operation no longer active");
        }
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

    fn max_request_body_bytes(&self) -> Option<usize> {
        Some(MAX_BUFFER)
    }

    async fn request(&self, request: http::Request<Bytes>) -> HttpResult {
        let (parts, bytes) = request.into_parts();
        if bytes.len() > MAX_BUFFER {
            return Err(HttpError::InvalidRequest(anyhow::anyhow!(
                "remote config request body exceeds transport limit"
            )));
        }
        let headers = parts
            .headers
            .iter()
            .map(|(name, value)| Ok((name.to_string(), value.to_str()?.to_string())))
            .collect::<Result<_>>()
            .map_err(HttpError::InvalidRequest)?;
        let (sender, receiver) = oneshot::channel();
        let _operation = self
            .start(Pending::Http(sender), |id| Event::Http {
                id,
                method: parts.method,
                url: parts.uri.to_string(),
                headers,
                bytes,
            })
            .map_err(HttpError::InvalidRequest)?;
        receiver
            .await
            .map_err(|error| HttpError::Network(error.into()))?
    }
}

impl SleepCapability for Host {
    fn new() -> Self {
        Self::default()
    }

    async fn sleep(&self, duration: Duration) {
        let delay_ms = u32::try_from(duration.as_millis()).unwrap_or(u32::MAX);
        let (sender, receiver) = oneshot::channel();
        if let Ok(_operation) =
            self.start(Pending::Timer(sender), |id| Event::Sleep { id, delay_ms })
        {
            let _completion = receiver.await;
        }
    }
}
