// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::client::Client;
use crate::host::{Event, MAX_BUFFER};
use crate::view::{Kind, Record, View};
use anyhow::{Context, Result, bail, ensure};
use bytes::Bytes;
use libdd_capabilities::HttpError;
use libdd_remote_config::fetch::{
    AssignedFile, ConfigApplyState, SessionPoll, WatchDecision, WatchOutcome, WatchOverflow,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::sync::Arc;

const MAX_BUFFER_BYTES: usize = 32 * 1024 * 1024;
const MAX_BUFFER_COUNT: usize = 256;
const MAX_VIEW_COUNT: usize = 32;
const MAX_CLIENT_COUNT: usize = 8;

enum Buffer {
    Input(Vec<u8>),
    Control(Vec<u8>),
    Body(Bytes),
    File {
        file: Arc<AssignedFile>,
        owner: Option<i32>,
    },
}

impl Buffer {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Input(bytes) | Self::Control(bytes) => bytes,
            Self::Body(bytes) => bytes,
            Self::File { file, .. } => file.bytes(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpReply {
    status: u16,
    #[serde(default)]
    headers: Vec<(String, String)>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PollReply {
    ready: bool,
    next_poll_after_ms: u32,
    batch: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    batch_fully_watched: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Default)]
struct PollDiagnostic(String);

impl PollDiagnostic {
    fn render(error: &anyhow::Error) -> String {
        let mut diagnostic = Self::default();
        let _ = write!(diagnostic, "{error:#}");
        diagnostic.0
    }
}

impl fmt::Write for PollDiagnostic {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        const MAX_BYTES: usize = 4096;
        let remaining = MAX_BYTES - self.0.len();
        if text.len() <= remaining {
            self.0.push_str(text);
            return Ok(());
        }
        self.0
            .push_str(&text[..text.floor_char_boundary(remaining)]);
        Err(fmt::Error)
    }
}

struct ApplyPlan<'plan> {
    records: &'plan [u8],
    outcome: WatchOutcome<'plan>,
}

impl<'plan> ApplyPlan<'plan> {
    fn read(bytes: &'plan [u8]) -> Result<Self> {
        let (&outcome, mut records) = bytes.split_first().context("missing plan outcome")?;
        let outcome = match outcome {
            0 => WatchOutcome::Applied,
            1 => WatchOutcome::Retained,
            2 => WatchOutcome::Failed(Self::diagnostic(&mut records)?),
            _ => bail!("invalid plan outcome"),
        };
        Ok(Self { records, outcome })
    }

    fn word(bytes: &mut &[u8]) -> Result<u32> {
        let (word, remaining) = bytes
            .split_first_chunk::<4>()
            .context("truncated plan word")?;
        *bytes = remaining;
        Ok(u32::from_le_bytes(*word))
    }

    fn diagnostic(bytes: &mut &'plan [u8]) -> Result<&'plan str> {
        let length = usize::try_from(Self::word(bytes)?)?;
        let (text, remaining) = bytes
            .split_at_checked(length)
            .context("truncated plan diagnostic")?;
        *bytes = remaining;
        std::str::from_utf8(text).context("invalid UTF-8 in plan diagnostic")
    }

    fn next(&mut self) -> Result<(i32, WatchDecision<'plan>)> {
        let index = i32::try_from(Self::word(&mut self.records)?).context("invalid view index")?;
        let (&decision, remaining) = self
            .records
            .split_first()
            .context("missing plan decision")?;
        self.records = remaining;
        let decision = match decision {
            0 => WatchDecision::Eligible,
            1 => WatchDecision::Ignored,
            2 => WatchDecision::Rejected(Self::diagnostic(&mut self.records)?),
            _ => bail!("invalid plan decision"),
        };
        Ok((index, decision))
    }

    fn project<'view, Entry>(
        mut self,
        view: &'view View,
        mut entry: impl FnMut(&'view Record, WatchDecision<'plan>, Option<&'plan str>) -> Entry,
    ) -> Result<Vec<Entry>> {
        let mut previous = None;
        let mut diagnostic_bytes = 0;
        let mut entries = Vec::with_capacity(view.record_count().min(self.records.len() / 5));
        while !self.records.is_empty() {
            let (index, decision) = self.next()?;
            ensure!(
                previous.is_none_or(|previous| previous < index),
                "plan indices must be unique and increasing"
            );
            previous = Some(index);
            let record = view.record(index)?;
            ensure!(
                record.kind == Kind::Upsert,
                "plans complete only upsert records"
            );
            let error = decision.diagnostic(self.outcome.failure());
            diagnostic_bytes += error.map_or(0, str::len);
            ensure!(
                diagnostic_bytes <= MAX_BUFFER,
                "plan diagnostic budget exhausted"
            );
            entries.push(entry(record, decision, error));
        }
        Ok(entries)
    }
}

#[derive(Default)]
struct Registry {
    clients: BTreeMap<i32, Client>,
    buffers: BTreeMap<i32, Buffer>,
    views: BTreeMap<i32, View>,
    buffer_bytes: usize,
    next: i32,
    error: String,
}

impl Registry {
    fn id(&mut self) -> Result<i32> {
        self.next = self.next.checked_add(1).context("resource IDs exhausted")?;
        Ok(self.next)
    }

    fn check_buffer(&self, length: usize) -> Result<()> {
        ensure!(length <= MAX_BUFFER, "buffer exceeds 16 MiB");
        ensure!(
            self.buffer_bytes + length <= MAX_BUFFER_BYTES,
            "buffer byte budget exhausted"
        );
        ensure!(
            self.buffers.len() < MAX_BUFFER_COUNT,
            "buffer count exhausted"
        );
        Ok(())
    }

    fn add_buffer(&mut self, buffer: Buffer) -> Result<i32> {
        let length = buffer.bytes().len();
        self.check_buffer(length)?;
        let id = self.id()?;
        self.buffers.insert(id, buffer);
        self.buffer_bytes += length;
        Ok(id)
    }

    fn remove_buffer(&mut self, handle: i32) -> Option<Buffer> {
        let buffer = self.buffers.remove(&handle)?;
        self.buffer_bytes -= buffer.bytes().len();
        Some(buffer)
    }

    fn buffer(&self, handle: i32) -> Result<&[u8]> {
        self.buffers
            .get(&handle)
            .map(Buffer::bytes)
            .context("invalid buffer")
    }

    fn read_json<T: DeserializeOwned>(&self, handle: i32) -> Result<T> {
        Ok(serde_json::from_slice(self.buffer(handle)?)?)
    }

    fn write_json(&mut self, value: &impl Serialize) -> Result<i32> {
        self.add_buffer(Buffer::Control(serde_json::to_vec(value)?))
    }

    fn client(&mut self, handle: i32) -> Result<&mut Client> {
        self.clients
            .get_mut(&handle)
            .context("invalid or closed client")
    }

    fn view(&self, handle: i32) -> Result<&View> {
        self.views.get(&handle).context("invalid or released view")
    }

    fn add_view(&mut self, view: View) -> Result<i32> {
        ensure!(self.views.len() < MAX_VIEW_COUNT, "view budget exhausted");
        let id = self.id()?;
        self.views.insert(id, view);
        Ok(id)
    }

    fn available_view_id(&self) -> Result<i32> {
        ensure!(self.views.len() < MAX_VIEW_COUNT, "view budget exhausted");
        self.next.checked_add(1).context("resource IDs exhausted")
    }

    fn write_event(&mut self, event: &Event) -> Result<i32> {
        let body = match event {
            Event::Http { bytes, .. } if !bytes.is_empty() => {
                self.add_buffer(Buffer::Body(bytes.clone()))?
            }
            _ => 0,
        };
        let result = self.write_json(&event.describe(body));
        if result.is_err() && body != 0 {
            self.remove_buffer(body);
        }
        result
    }

    fn write_result(
        &mut self,
        owner: i32,
        ready: bool,
        fully_watched: bool,
        poll: &SessionPoll,
    ) -> Result<i32> {
        let batch = match &poll.result {
            Ok(Some(batch)) => self.add_view(View::batch(owner, batch))?,
            _ => 0,
        };
        let result = self.write_json(&PollReply {
            ready,
            next_poll_after_ms: poll.next_poll_after.as_millis().min(i32::MAX as u128) as u32,
            batch,
            batch_fully_watched: fully_watched.then_some(true),
            error: poll.result.as_ref().err().map(PollDiagnostic::render),
        });
        if result.is_err() && batch != 0 {
            self.views.remove(&batch);
        }
        result
    }

    fn take_body(&mut self, handle: i32) -> Result<Bytes> {
        if handle == 0 {
            return Ok(Bytes::new());
        }
        ensure!(
            matches!(self.buffers.get(&handle), Some(Buffer::Input(_))),
            "reply body needs an input buffer"
        );
        match self.remove_buffer(handle) {
            Some(Buffer::Input(bytes)) => Ok(Bytes::from(bytes)),
            _ => bail!("invalid reply body"),
        }
    }
}

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
}

fn entry(operation: impl FnOnce(&mut Registry) -> Result<i32>) -> i32 {
    REGISTRY.with_borrow_mut(|registry| match operation(registry) {
        Ok(value) => value,
        Err(error) => {
            registry.error = format!("{error:#}");
            -1
        }
    })
}

pub fn rc_version() -> i32 {
    crate::ABI_VERSION
}

pub fn rc_alloc(length: i32) -> i32 {
    entry(|registry| {
        let length = usize::try_from(length)?;
        registry.check_buffer(length)?;
        registry.add_buffer(Buffer::Input(vec![0; length]))
    })
}

pub fn rc_buffer_ptr(buffer: i32) -> i32 {
    entry(|registry| Ok(registry.buffer(buffer)?.as_ptr() as i32))
}

pub fn rc_buffer_len(buffer: i32) -> i32 {
    entry(|registry| Ok(i32::try_from(registry.buffer(buffer)?.len())?))
}

pub fn rc_free(buffer: i32) -> i32 {
    entry(|registry| {
        registry
            .remove_buffer(buffer)
            .context("invalid or freed buffer")?;
        Ok(0)
    })
}

pub fn rc_error() -> i32 {
    entry(|registry| registry.add_buffer(Buffer::Control(registry.error.as_bytes().to_vec())))
}

pub fn rc_open(options: i32) -> i32 {
    entry(|registry| {
        ensure!(
            registry.clients.len() < MAX_CLIENT_COUNT,
            "client budget exhausted"
        );
        let client = Client::new(registry.read_json(options)?)?;
        let handle = registry.id()?;
        registry.clients.insert(handle, client);
        Ok(handle)
    })
}

pub fn rc_set_interest(client: i32, options: i32) -> i32 {
    entry(|registry| {
        let interest = registry.read_json(options)?;
        registry.client(client)?.set_interest(interest)?;
        Ok(0)
    })
}

pub fn rc_set_identity(client: i32, options: i32) -> i32 {
    entry(|registry| {
        let identity = registry.read_json(options)?;
        registry.client(client)?.set_identity(identity)?;
        Ok(0)
    })
}

pub fn rc_retarget(client: i32, options: i32) -> i32 {
    entry(|registry| {
        let options = registry.read_json(options)?;
        registry.client(client)?.retarget(options)
    })
}

pub fn rc_poll(client: i32) -> i32 {
    entry(|registry| {
        registry.client(client)?.start()?;
        Ok(0)
    })
}

pub fn rc_event(client: i32) -> i32 {
    entry(|registry| {
        let host = registry.client(client)?.host.clone();
        let Some(event) = host.event() else {
            return Ok(0);
        };
        let result = registry.write_event(&event);
        if result.is_err() {
            host.restore_event(event);
        }
        result
    })
}

pub fn rc_http_succeed(client: i32, operation: i32, response: i32, body: i32) -> i32 {
    entry(|registry| {
        let reply: HttpReply = registry.read_json(response)?;
        let mut response = http::Response::builder().status(reply.status);
        for (name, value) in reply.headers {
            response = response.header(name, value);
        }
        let response = response.body(())?;
        let host = registry.client(client)?.host.clone();
        ensure!(host.expects_http(operation), "not a pending HTTP operation");
        let bytes = registry.take_body(body)?;
        host.complete_http(operation, Ok(response.map(|()| bytes)))?;
        registry.client(client)?.drive();
        Ok(0)
    })
}

pub fn rc_http_fail(client: i32, operation: i32, message: i32) -> i32 {
    entry(|registry| {
        let error = std::str::from_utf8(registry.buffer(message)?)?.to_owned();
        let client = registry.client(client)?;
        client
            .host
            .complete_http(operation, Err(HttpError::Network(anyhow::anyhow!(error))))?;
        client.drive();
        Ok(0)
    })
}

pub fn rc_timer_elapsed(client: i32, operation: i32) -> i32 {
    entry(|registry| {
        let client = registry.client(client)?;
        client.host.complete_timer(operation)?;
        client.drive();
        Ok(0)
    })
}

pub fn rc_result(client: i32) -> i32 {
    entry(|registry| {
        let slot = registry.client(client)?;
        let Some(poll) = slot.result.as_ref() else {
            return Ok(0);
        };
        let (ready, fully_watched) = slot.result_flags(poll)?;
        let poll = slot.result.take().context("missing poll result")?;
        let result = registry.write_result(client, ready, fully_watched, &poll);
        if result.is_err() {
            registry.client(client)?.result = Some(poll);
        }
        result
    })
}

pub fn rc_snapshot(client: i32) -> i32 {
    entry(|registry| match registry.client(client)?.snapshot()? {
        Some(desired) => registry.add_view(View::snapshot(client, desired)),
        None => Ok(0),
    })
}

pub fn rc_view_snapshot(view: i32) -> i32 {
    entry(|registry| {
        let view = registry.view(view)?;
        registry.add_view(view.desired_view())
    })
}

pub fn rc_view_describe(view: i32) -> i32 {
    entry(|registry| {
        let descriptor = serde_json::to_vec(&registry.view(view)?.describe())?;
        registry.add_buffer(Buffer::Control(descriptor))
    })
}

pub fn rc_view_file(view: i32, index: i32, previous: i32) -> i32 {
    entry(|registry| {
        let view = registry.view(view)?;
        let record = view.record(index)?;
        let file = match previous {
            0 => &record.file,
            1 => match record.previous.as_ref() {
                Some(file) => file,
                None => return Ok(0),
            },
            _ => bail!("previous must be 0 or 1"),
        };
        registry.add_buffer(Buffer::File {
            file: file.clone(),
            owner: (view.grants_receipts() && previous == 0 && record.kind != Kind::Remove)
                .then_some(view.owner),
        })
    })
}

pub fn rc_view_release(view: i32) -> i32 {
    entry(|registry| {
        ensure!(
            registry.view(view)?.watch.is_none(),
            "finish a watch update to release its view"
        );
        registry
            .views
            .remove(&view)
            .context("invalid or released view")?;
        Ok(0)
    })
}

fn apply_state(state: i32, message: impl FnOnce() -> Result<String>) -> Result<ConfigApplyState> {
    Ok(match state {
        1 => ConfigApplyState::Unacknowledged,
        2 => ConfigApplyState::Acknowledged,
        3 => ConfigApplyState::Error(message()?),
        _ => bail!("invalid apply state"),
    })
}

pub fn rc_apply(client: i32, receipt: i32, state: i32, message: i32) -> i32 {
    entry(|registry| {
        let Some(Buffer::File {
            file,
            owner: Some(owner),
        }) = registry.buffers.get(&receipt)
        else {
            return Ok(0);
        };
        if *owner != client {
            return Ok(0);
        }
        let state = apply_state(state, || {
            Ok(std::str::from_utf8(registry.buffer(message)?)?.to_owned())
        })?;
        let Some(client) = registry.clients.get(&client) else {
            return Ok(0);
        };
        Ok(i32::from(client.apply(file, state)?))
    })
}

pub fn rc_apply_plan(client: i32, view: i32, plan: i32) -> i32 {
    entry(|registry| {
        let view = registry.view(view)?;
        let Some(client) = registry
            .clients
            .get(&client)
            .filter(|_| view.owner == client && view.grants_receipts())
        else {
            return Ok(0);
        };
        let files = ApplyPlan::read(registry.buffer(plan)?)?
            .project(view, |record, _, error| (record.file.as_ref(), error))?;
        let states = files.into_iter().map(|(file, error)| {
            let state = error.map_or(ConfigApplyState::Acknowledged, |error| {
                ConfigApplyState::Error(error.to_owned())
            });
            (file, state)
        });
        Ok(i32::try_from(client.apply_many(states)?)?)
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawReceiptPlan {
    receipt_owner: i32,
    outcomes: Vec<RawReceiptOutcome>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReceiptOutcome {
    path: String,
    generation: String,
    state: i32,
    #[serde(default)]
    message: String,
}

pub fn rc_apply_receipts(client: i32, plan: i32) -> i32 {
    entry(|registry| {
        let Some(owner) = registry.clients.get(&client) else {
            return Ok(0);
        };
        let plan: RawReceiptPlan = registry.read_json(plan)?;
        if plan.receipt_owner != client {
            return Ok(0);
        }
        let outcomes = plan
            .outcomes
            .into_iter()
            .map(|outcome| {
                Ok((
                    outcome.path,
                    outcome
                        .generation
                        .parse()
                        .context("invalid receipt generation")?,
                    apply_state(outcome.state, || Ok(outcome.message))?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(i32::try_from(owner.apply_receipts(outcomes)?)?)
    })
}

pub fn rc_watch_open(client: i32, options: i32) -> i32 {
    entry(|registry| {
        let options = registry.read_json(options)?;
        registry.client(client)?.open_watch(options)
    })
}

pub fn rc_watch_next(client: i32, watch: i32) -> i32 {
    entry(|registry| {
        let slot = registry
            .clients
            .get(&client)
            .context("invalid or closed client")?;
        let mut handle = 0;
        match slot.next_watch(watch, || {
            handle = registry.available_view_id()?;
            Ok(())
        }) {
            Ok(Some(update)) => {
                registry.next = handle;
                registry
                    .views
                    .insert(handle, View::watch(client, watch, update));
                Ok(handle)
            }
            Ok(None) => Ok(0),
            Err(error) if error.is::<WatchOverflow>() => Ok(-2),
            Err(error) => Err(error),
        }
    })
}

pub fn rc_watch_replay(client: i32, watch: i32) -> i32 {
    entry(
        |registry| match registry.client(client)?.replay_watch(watch) {
            Ok(queued) => Ok(i32::from(queued)),
            Err(error) if error.is::<WatchOverflow>() => Ok(-2),
            Err(error) => Err(error),
        },
    )
}

pub fn rc_watch_finish(client: i32, watch: i32, view: i32, plan: i32) -> i32 {
    entry(|registry| {
        let update = registry.view(view)?;
        let ticket = update
            .watch
            .as_ref()
            .filter(|ticket| update.owner == client && ticket.watch == watch)
            .context("view does not belong to the watch")?;
        let plan = ApplyPlan::read(registry.buffer(plan)?)?;
        let outcome = plan.outcome;
        let decisions = plan.project(update, |_, decision, _| decision)?;
        let slot = registry
            .clients
            .get(&client)
            .context("invalid or closed client")?;
        let count = slot.finish_watch(watch, ticket.ticket, outcome, &decisions)?;
        registry.views.remove(&view);
        Ok(i32::try_from(count)?)
    })
}

pub fn rc_watch_close(client: i32, watch: i32) -> i32 {
    entry(|registry| Ok(i32::from(registry.client(client)?.close_watch(watch)?)))
}

pub fn rc_cancel(client: i32) -> i32 {
    entry(|registry| {
        registry.client(client)?.cancel()?;
        Ok(0)
    })
}

pub fn rc_close(client: i32) -> i32 {
    entry(|registry| {
        registry
            .clients
            .remove(&client)
            .context("invalid or closed client")?;
        registry
            .views
            .retain(|_, view| view.owner != client || view.watch.is_none());
        Ok(0)
    })
}
