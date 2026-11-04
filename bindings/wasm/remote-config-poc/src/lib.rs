// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod generated;
mod host;

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures::task::noop_waker;
use libdd_common::Endpoint;
use libdd_remote_config::fetch::{
    AgentlessConfig, ConfigApplyState, ConfigInvariants, ConfigOptions, SingleChangesFetcher,
};
use libdd_remote_config::file_change_tracker::{Change, FilePath};
use libdd_remote_config::file_storage::SimpleFileStorage;
use libdd_remote_config::{
    RemoteConfigCapabilities, RemoteConfigPath, RemoteConfigProduct, Target,
};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context as TaskContext, Poll};

type Fetcher = SingleChangesFetcher<SimpleFileStorage, host::Host>;
type FetcherSlot = Rc<RefCell<Option<FetcherState>>>;
type PollFuture = Pin<Box<dyn Future<Output = Result<Vec<ChangeRecord>>>>>;
const MAX_BUFFER: usize = 16 * 1024 * 1024;
const MAX_BUFFERS: usize = 32 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Options {
    client_id: String,
    runtime_id: String,
    service: String,
    env: String,
    app_version: String,
    language: String,
    tracer_version: String,
    url: String,
    timeout_ms: u64,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    hostname: Option<String>,
    #[serde(default)]
    config_root: Option<String>,
    #[serde(default)]
    director_root: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    process_tags: Vec<String>,
    products: Vec<String>,
    #[serde(default)]
    capabilities: Vec<RemoteConfigCapabilities>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChangeRecord {
    kind: &'static str,
    path: String,
    // Decimal text preserves all u64 versions in JavaScript as well as C#.
    version: String,
    contents_base64: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Acknowledgement {
    path: String,
    state: u32,
    #[serde(default)]
    error: String,
}

#[derive(Serialize)]
#[serde(untagged)]
enum Outcome {
    Success { changes: Vec<ChangeRecord> },
    Error { error: String },
}

#[derive(Clone)]
struct Setup {
    target: Target,
    runtime_id: String,
    client_id: String,
    options: ConfigOptions,
}

enum FetcherState {
    Uninitialized(Box<Setup>),
    Ready(Box<Fetcher>),
}

struct Lease {
    fetcher: Option<FetcherState>,
    slot: FetcherSlot,
}

impl Drop for Lease {
    fn drop(&mut self) {
        // The slot is never borrowed across a guest poll. Cancellation returns ownership here.
        self.slot.replace(self.fetcher.take());
    }
}

struct Client {
    slot: FetcherSlot,
    host: host::Host,
    pending: Option<PollFuture>,
    outcome: Option<Outcome>,
}

impl Client {
    fn new(options: Options) -> Result<Self> {
        ensure!(
            options.timeout_ms <= 2_147_483_647,
            "timeout exceeds the common host timer range"
        );
        let url = libdd_common::parse_uri(&options.url)?;
        ensure!(
            matches!(url.scheme_str(), Some("http" | "https")) && url.authority().is_some(),
            "PoC requires an absolute HTTP(S) remote configuration URL"
        );
        let endpoint = Endpoint {
            url,
            timeout_ms: options.timeout_ms,
            api_key: options.api_key.map(Into::into),
            ..Default::default()
        };
        let agentless = if endpoint.api_key.is_some() {
            ensure!(
                endpoint.api_key.as_ref().is_some_and(|key| !key.is_empty()),
                "API key must not be empty"
            );
            let mut config = AgentlessConfig::new(options.hostname.unwrap_or_default(), &endpoint)?;
            if let Some(root) = options.config_root {
                config = config.with_config_root(root.into_bytes());
            }
            if let Some(root) = options.director_root {
                config = config.with_director_root(root.into_bytes());
            }
            Some(config)
        } else {
            ensure!(
                options.hostname.is_none()
                    && options.config_root.is_none()
                    && options.director_root.is_none(),
                "agentless options require an API key"
            );
            None
        };
        let host = host::Host::default();
        let setup = Setup {
            target: Target::new(
                options.service,
                options.env,
                options.app_version,
                options.tags,
                options.process_tags,
            ),
            runtime_id: options.runtime_id,
            client_id: options.client_id,
            options: ConfigOptions {
                invariants: ConfigInvariants {
                    language: options.language,
                    tracer_version: options.tracer_version,
                    endpoint,
                    agentless,
                },
                products: options
                    .products
                    .into_iter()
                    .map(|product| {
                        product
                            .parse()
                            .map_err(|_| anyhow::anyhow!("invalid product {product}"))
                    })
                    .collect::<Result<Vec<RemoteConfigProduct>>>()?,
                capabilities: options.capabilities,
            },
        };
        let fetcher = FetcherState::Uninitialized(Box::new(setup));
        Ok(Self {
            slot: Rc::new(RefCell::new(Some(fetcher))),
            host,
            pending: None,
            outcome: None,
        })
    }

    fn start(&mut self) -> Result<()> {
        ensure!(self.pending.is_none(), "poll already active");
        ensure!(self.outcome.is_none(), "previous result has not been read");
        let fetcher = self.slot.borrow_mut().take().context("client is busy")?;
        let mut lease = Lease {
            fetcher: Some(fetcher),
            slot: self.slot.clone(),
        };
        let host = self.host.clone();
        self.pending = Some(Box::pin(async move {
            if let Some(FetcherState::Uninitialized(setup)) = &lease.fetcher {
                // Keep the setup in the lease until asynchronous initialization succeeds.
                let setup = setup.clone();
                let fetcher = SingleChangesFetcher::new(
                    SimpleFileStorage::default(),
                    setup.target,
                    setup.runtime_id,
                    setup.options,
                    host,
                )
                .await?
                .with_client_id(setup.client_id);
                lease.fetcher = Some(FetcherState::Ready(Box::new(fetcher)));
            }
            let Some(FetcherState::Ready(fetcher)) = lease.fetcher.as_mut() else {
                bail!("missing initialized fetcher")
            };
            let changes = fetcher.fetch_changes::<Vec<u8>>().await?;
            Ok(changes
                .into_iter()
                .map(|change| {
                    let (kind, file, removed) = match change {
                        Change::Add(file) => ("add", file, false),
                        Change::Update(file, _) => ("update", file, false),
                        Change::Remove(file) => ("remove", file, true),
                    };
                    let contents_base64 =
                        (!removed).then(|| STANDARD.encode(file.contents().as_slice()));
                    ChangeRecord {
                        kind,
                        path: file.path().to_string(),
                        version: file.version().to_string(),
                        contents_base64,
                    }
                })
                .collect())
        }));
        self.drive();
        Ok(())
    }

    fn drive(&mut self) {
        let Some(pending) = &mut self.pending else {
            return;
        };
        if let Poll::Ready(result) = pending
            .as_mut()
            .poll(&mut TaskContext::from_waker(&noop_waker()))
        {
            self.pending = None;
            self.outcome = Some(match result {
                Ok(changes) => Outcome::Success { changes },
                Err(error) => Outcome::Error {
                    error: format!("{error:#}"),
                },
            });
        }
    }

    fn ack(&mut self, acknowledgement: Acknowledgement) -> Result<()> {
        ensure!(
            self.pending.is_none(),
            "cannot acknowledge during an active poll"
        );
        let state = match acknowledgement.state {
            1 => ConfigApplyState::Unacknowledged,
            2 => ConfigApplyState::Acknowledged,
            3 => ConfigApplyState::Error(acknowledgement.error),
            _ => bail!("invalid apply state"),
        };
        let slot = self.slot.borrow();
        let Some(FetcherState::Ready(fetcher)) = slot.as_ref() else {
            bail!("client is not initialized")
        };
        fetcher
            .fetcher
            .set_config_state(&RemoteConfigPath::parse(&acknowledgement.path)?, state);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_client_owns_the_timeout() {
        let mut options: serde_json::Value =
            serde_json::from_str(include_str!("../test/scenario.json")).unwrap();
        options["options"]["language"] = "rust".into();
        options["options"]["url"] = "http://127.0.0.1:8126".into();
        let mut client =
            Client::new(serde_json::from_value(options["options"].take()).unwrap()).unwrap();
        client.start().unwrap();
        let events = client.host.events().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, host::Event::Http { .. }))
                .count(),
            1
        );
        let timer = events
            .iter()
            .find_map(|event| match event {
                host::Event::Sleep { id, .. } => Some(*id),
                _ => None,
            })
            .expect("the injected host must receive the timeout operation");
        client
            .host
            .complete(timer, serde_json::from_str("{}").unwrap())
            .unwrap();
        client.drive();
        assert!(
            matches!(client.outcome, Some(Outcome::Error { ref error }) if error.contains("timed out"))
        );
        assert!(
            client.slot.borrow().is_some(),
            "completion must return fetcher ownership"
        );
    }
}

#[derive(Default)]
struct Registry {
    next: i32,
    buffers: HashMap<i32, Vec<u8>>,
    buffer_bytes: usize,
    clients: HashMap<i32, Client>,
    error: String,
}

impl Registry {
    fn id(&mut self) -> Result<i32> {
        self.next = self.next.checked_add(1).context("resource IDs exhausted")?;
        Ok(self.next)
    }

    fn buffer(&mut self, bytes: Vec<u8>) -> Result<i32> {
        ensure!(bytes.len() <= MAX_BUFFER, "buffer exceeds 16 MiB");
        ensure!(
            self.buffer_bytes + bytes.len() <= MAX_BUFFERS,
            "buffer budget exceeds 32 MiB"
        );
        let id = self.id()?;
        self.buffer_bytes += bytes.len();
        self.buffers.insert(id, bytes);
        Ok(id)
    }

    fn decode<T: serde::de::DeserializeOwned>(&self, buffer: i32) -> Result<T> {
        Ok(serde_json::from_slice(
            self.buffers.get(&buffer).context("invalid buffer")?,
        )?)
    }

    fn client(&mut self, client: i32) -> Result<&mut Client> {
        self.clients
            .get_mut(&client)
            .context("invalid or closed client")
    }
}

thread_local! {
    // WebAssembly memory isolates this registry per instance; it is not host-process state.
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

mod api {
    use super::*;

    pub fn rc_version() -> i32 {
        generated::ABI_VERSION
    }

    pub fn rc_alloc(length: i32) -> i32 {
        entry(|registry| {
            let length = usize::try_from(length)?;
            ensure!(
                length <= MAX_BUFFER && registry.buffer_bytes + length <= MAX_BUFFERS,
                "buffer budget exceeded"
            );
            registry.buffer(vec![0; length])
        })
    }

    pub fn rc_buffer_ptr(buffer: i32) -> i32 {
        entry(|registry| {
            let bytes = registry.buffers.get(&buffer).context("invalid buffer")?;
            // The ABI returns the wasm32 address bits in an i32, including its high bit.
            let address = u32::try_from(bytes.as_ptr() as usize).context("not a wasm32 address")?;
            Ok(i32::from_ne_bytes(address.to_ne_bytes()))
        })
    }

    pub fn rc_buffer_len(buffer: i32) -> i32 {
        entry(|registry| {
            Ok(i32::try_from(
                registry
                    .buffers
                    .get(&buffer)
                    .context("invalid buffer")?
                    .len(),
            )?)
        })
    }

    pub fn rc_free(buffer: i32) -> i32 {
        entry(|registry| {
            let bytes = registry
                .buffers
                .remove(&buffer)
                .context("invalid or freed buffer")?;
            registry.buffer_bytes -= bytes.len();
            Ok(0)
        })
    }

    pub fn rc_error() -> i32 {
        entry(|registry| registry.buffer(registry.error.as_bytes().to_vec()))
    }

    pub fn rc_create(options: i32) -> i32 {
        entry(|registry| {
            let client = Client::new(registry.decode(options)?)?;
            let id = registry.id()?;
            registry.clients.insert(id, client);
            Ok(id)
        })
    }

    pub fn rc_poll(client: i32) -> i32 {
        entry(|registry| {
            registry.client(client)?.start()?;
            Ok(0)
        })
    }

    pub fn rc_events(client: i32) -> i32 {
        entry(|registry| {
            let events = registry.client(client)?.host.events()?;
            registry.buffer(serde_json::to_vec(&events)?)
        })
    }

    pub fn rc_complete(client: i32, operation: i32, reply: i32) -> i32 {
        entry(|registry| {
            let reply = registry.decode(reply)?;
            let client = registry.client(client)?;
            client.host.complete(operation, reply)?;
            client.drive();
            Ok(0)
        })
    }

    pub fn rc_result(client: i32) -> i32 {
        entry(|registry| match registry.client(client)?.outcome.take() {
            Some(outcome) => registry.buffer(serde_json::to_vec(&outcome)?),
            None => Ok(0),
        })
    }

    pub fn rc_ack(client: i32, acknowledgement: i32) -> i32 {
        entry(|registry| {
            let acknowledgement = registry.decode(acknowledgement)?;
            registry.client(client)?.ack(acknowledgement)?;
            Ok(0)
        })
    }

    pub fn rc_cancel(client: i32) -> i32 {
        entry(|registry| {
            let client = registry.client(client)?;
            ensure!(client.pending.is_some(), "no active poll");
            client.pending = None;
            client.outcome = Some(Outcome::Error {
                error: "cancelled".into(),
            });
            Ok(0)
        })
    }

    pub fn rc_close(client: i32) -> i32 {
        entry(|registry| {
            registry
                .clients
                .remove(&client)
                .context("invalid or closed client")?;
            Ok(0)
        })
    }
}
