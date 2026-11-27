// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::host::Host;
use crate::shared::Shared;
use anyhow::{Context, Result, ensure};
use futures_util::task::noop_waker_ref;
use libdd_capabilities::MaybeSendFuture;
use libdd_common::{Endpoint, parse_uri};
use libdd_remote_config::fetch::{
    AgentlessConfig, AssignedFile, Assignment, ConfigApplyState, ConfigInvariants, ConfigOptions,
    Session, SessionPoll, WatchDecision, WatchMode, WatchOutcome, WatchRegistry, WatchUpdate,
};
use libdd_remote_config::{RemoteConfigCapabilities, RemoteConfigProduct, Target};
use serde::Deserialize;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

const START_INTERVAL: Duration = Duration::from_secs(5);
type SessionSlot = Shared<SessionState>;
type PollFuture = Pin<Box<dyn MaybeSendFuture<SessionPoll>>>;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Identity {
    client_id: String,
    runtime_id: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    extra_services: Vec<String>,
    process_tags: Option<Vec<String>>,
}

impl Identity {
    fn apply_process_tags(&mut self, target: &mut TargetOptions) {
        if let Some(process_tags) = self.process_tags.take() {
            target.process_tags = process_tags;
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(try_from = "InterestOptions")]
pub struct Interest {
    products: Vec<RemoteConfigProduct>,
    capabilities: Vec<RemoteConfigCapabilities>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InterestOptions {
    #[serde(deserialize_with = "parse_products")]
    products: Vec<RemoteConfigProduct>,
    #[serde(default)]
    capabilities: Vec<RemoteConfigCapabilities>,
    #[serde(default)]
    capability_indices: Vec<u8>,
}

impl TryFrom<InterestOptions> for Interest {
    type Error = anyhow::Error;

    fn try_from(mut options: InterestOptions) -> Result<Self> {
        if !options.capability_indices.is_empty() {
            ensure!(
                options.capabilities.is_empty(),
                "use capability names or indices, not both"
            );
            ensure!(
                options.capability_indices.len() <= 256,
                "too many capability indices"
            );
            options
                .capabilities
                .reserve(options.capability_indices.len());
            for index in options.capability_indices {
                let capability = RemoteConfigCapabilities::from_index(index)
                    .with_context(|| format!("unsupported capability index {index}"))?;
                options.capabilities.push(capability);
            }
        }

        Ok(Self {
            products: options.products,
            capabilities: options.capabilities,
        })
    }
}

fn parse_products<'de, D: serde::Deserializer<'de>>(
    source: D,
) -> Result<Vec<RemoteConfigProduct>, D::Error> {
    Vec::<String>::deserialize(source)?
        .into_iter()
        .map(|name| {
            name.parse()
                .map_err(|_| serde::de::Error::custom(format!("unsupported product {name}")))
        })
        .collect()
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TargetOptions {
    service: String,
    env: String,
    app_version: String,
    #[serde(default)]
    process_tags: Vec<String>,
}

impl TargetOptions {
    fn into_target(self, tags: Vec<String>) -> Target {
        Target::new(
            self.service,
            self.env,
            self.app_version,
            tags,
            self.process_tags,
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetargetOptions {
    target: TargetOptions,
    identity: Identity,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenOptions {
    target: TargetOptions,
    identity: Identity,
    interest: Interest,
    language: String,
    tracer_version: String,
    url: String,
    timeout_ms: u32,
    api_key: Option<String>,
    hostname: Option<String>,
    config_root: Option<String>,
    director_root: Option<String>,
}

#[derive(Clone)]
struct Setup {
    target: TargetOptions,
    identity: Identity,
    options: ConfigOptions,
}

impl Setup {
    async fn open(self, host: Host) -> Result<Session<Host>> {
        let target = self.target.into_target(self.identity.tags);
        let mut session = Session::open(
            target,
            self.identity.runtime_id,
            self.identity.client_id,
            self.options,
            host,
        )
        .await?;
        session.set_extra_services(self.identity.extra_services);
        Ok(session)
    }
}

#[derive(Default)]
struct SessionState {
    session: Option<Session<Host>>,
    initial_watches: WatchRegistry,
}

struct Lease {
    slot: SessionSlot,
    state: SessionState,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.slot
            .write(|slot| *slot = std::mem::take(&mut self.state));
    }
}

#[derive(Deserialize)]
#[serde(tag = "delivery", rename_all = "camelCase", deny_unknown_fields)]
pub enum WatchOptions {
    LatestState {
        #[serde(deserialize_with = "parse_products")]
        products: Vec<RemoteConfigProduct>,
    },
    OrderedState {
        #[serde(deserialize_with = "parse_products")]
        products: Vec<RemoteConfigProduct>,
        #[serde(rename = "maxPending")]
        max_pending: usize,
    },
}

pub struct Client {
    pub host: Host,
    pub result: Option<SessionPoll>,
    setup: Option<Setup>,
    slot: SessionSlot,
    pending: Option<PollFuture>,
}

impl Client {
    pub fn new(mut options: OpenOptions) -> Result<Self> {
        options.identity.apply_process_tags(&mut options.target);
        ensure!(
            options.timeout_ms <= i32::MAX as u32,
            "timeout exceeds the common host timer range"
        );
        let url = parse_uri(&options.url)?;
        ensure!(
            matches!(url.scheme_str(), Some("http" | "https")) && url.authority().is_some(),
            "remote configuration requires an absolute HTTP(S) URL"
        );
        let endpoint = Endpoint {
            url,
            timeout_ms: u64::from(options.timeout_ms),
            api_key: options.api_key.map(Into::into),
            ..Default::default()
        };
        let agentless = if let Some(key) = &endpoint.api_key {
            ensure!(!key.is_empty(), "API key must not be empty");
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
        Ok(Self {
            setup: Some(Setup {
                target: options.target,
                identity: options.identity,
                options: ConfigOptions {
                    invariants: ConfigInvariants {
                        language: options.language,
                        tracer_version: options.tracer_version,
                        endpoint,
                        agentless,
                    },
                    products: options.interest.products,
                    capabilities: options.interest.capabilities,
                },
            }),
            slot: SessionSlot::default(),
            host: Host::default(),
            pending: None,
            result: None,
        })
    }

    fn ensure_idle(&self) -> Result<()> {
        ensure!(self.pending.is_none(), "poll is in progress");
        Ok(())
    }

    pub fn set_interest(&mut self, interest: Interest) -> Result<()> {
        self.ensure_idle()?;
        if let Some(setup) = &mut self.setup {
            setup.options.products = interest.products;
            setup.options.capabilities = interest.capabilities;
        } else {
            self.slot.write(|slot| {
                if let Some(session) = slot.session.as_mut() {
                    session.set_interest(interest.products, interest.capabilities);
                }
            });
        }
        Ok(())
    }

    pub fn set_identity(&mut self, mut identity: Identity) -> Result<()> {
        self.ensure_idle()?;
        if let Some(setup) = &mut self.setup {
            identity.apply_process_tags(&mut setup.target);
            setup.identity = identity;
        } else {
            self.slot.write(|slot| {
                if let Some(session) = slot.session.as_mut() {
                    session.set_identity(
                        identity.client_id,
                        identity.runtime_id,
                        identity.tags,
                        identity.extra_services,
                        identity.process_tags,
                    );
                }
            });
        }
        Ok(())
    }

    pub fn retarget(&mut self, mut options: RetargetOptions) -> Result<i32> {
        self.ensure_idle()?;
        ensure!(
            self.result.is_none(),
            "previous poll result has not been read"
        );
        options.identity.apply_process_tags(&mut options.target);
        if let Some(setup) = &mut self.setup {
            setup.target = options.target;
            setup.identity = options.identity;
            return Ok(1);
        }
        self.slot.write(|slot| {
            let session = slot
                .session
                .as_mut()
                .context("session is not initialized")?;
            ensure!(
                session.target_generation() < i32::MAX as u64,
                "target generation exhausted"
            );
            let generation = session.retarget(options.target.into_target(Vec::new()))?;
            session.set_identity(
                options.identity.client_id,
                options.identity.runtime_id,
                options.identity.tags,
                options.identity.extra_services,
                None,
            );
            Ok(i32::try_from(generation)?)
        })
    }

    pub fn start(&mut self) -> Result<()> {
        self.ensure_idle()?;
        ensure!(
            self.result.is_none(),
            "previous poll result has not been read"
        );
        let state = self.slot.write(|slot| {
            ensure!(
                slot.session.is_some() || self.setup.is_some(),
                "session is not initialized"
            );
            Ok(std::mem::take(slot))
        })?;
        let lease = Lease {
            slot: self.slot.clone(),
            state,
        };
        let setup = self.setup.clone();
        let host = self.host.clone();
        self.pending = Some(Box::pin(async move {
            let mut lease = lease;
            if let Some(setup) = setup {
                match setup.open(host).await {
                    Ok(mut session) => {
                        if let Err(error) =
                            session.install_initial_watches(&mut lease.state.initial_watches)
                        {
                            return SessionPoll {
                                result: Err(error),
                                next_poll_after: START_INTERVAL,
                            };
                        }
                        lease.state.session = Some(session);
                    }
                    Err(error) => {
                        return SessionPoll {
                            result: Err(error),
                            next_poll_after: START_INTERVAL,
                        };
                    }
                }
            }
            match lease.state.session.as_mut() {
                Some(session) => session.poll().await,
                None => SessionPoll {
                    result: Err(anyhow::anyhow!("session is not initialized")),
                    next_poll_after: START_INTERVAL,
                },
            }
        }));
        self.drive();
        Ok(())
    }

    pub fn drive(&mut self) {
        if let Some(pending) = &mut self.pending
            && let Poll::Ready(result) = pending
                .as_mut()
                .poll(&mut TaskContext::from_waker(noop_waker_ref()))
        {
            self.pending = None;
            self.result = Some(result);
            if self.slot.read(|slot| slot.session.is_some()) {
                self.setup = None;
            }
        }
    }

    pub fn result_flags(&self, poll: &SessionPoll) -> Result<(bool, bool)> {
        self.ensure_idle()?;
        Ok(self.slot.read(|slot| {
            let Some(session) = slot.session.as_ref() else {
                return (false, false);
            };
            let fully_watched = poll
                .result
                .as_ref()
                .ok()
                .and_then(Option::as_ref)
                .is_some_and(|batch| session.batch_fully_watched(batch));
            (session.snapshot().is_some(), fully_watched)
        }))
    }

    pub fn snapshot(&self) -> Result<Option<Arc<Assignment>>> {
        self.ensure_idle()?;
        Ok(self
            .slot
            .read(|slot| slot.session.as_ref().and_then(Session::snapshot).cloned()))
    }

    pub fn apply(&self, file: &AssignedFile, state: ConfigApplyState) -> Result<bool> {
        Ok(self.apply_many([(file, state)])? != 0)
    }

    pub fn apply_many<'file>(
        &self,
        files: impl IntoIterator<Item = (&'file AssignedFile, ConfigApplyState)>,
    ) -> Result<usize> {
        self.count_applied(|session| {
            files
                .into_iter()
                .filter_map(|(file, state)| session.complete(file, state).then_some(()))
                .count()
        })
    }

    pub fn apply_receipts(
        &self,
        receipts: impl IntoIterator<Item = (String, u64, ConfigApplyState)>,
    ) -> Result<usize> {
        self.count_applied(|session| {
            receipts
                .into_iter()
                .filter_map(|(path, generation, state)| {
                    session
                        .complete_observed(&path, generation, state)
                        .then_some(())
                })
                .count()
        })
    }

    fn count_applied(&self, apply: impl FnOnce(&Session<Host>) -> usize) -> Result<usize> {
        self.ensure_idle()?;
        Ok(self
            .slot
            .read(|slot| slot.session.as_ref().map_or(0, apply)))
    }

    pub fn cancel(&mut self) -> Result<()> {
        drop(self.pending.take().context("no active poll")?);
        if self.slot.read(|slot| slot.session.is_some()) {
            self.setup = None;
        }
        self.result = Some(SessionPoll {
            result: Err(anyhow::anyhow!("poll cancelled")),
            next_poll_after: START_INTERVAL,
        });
        Ok(())
    }

    pub fn open_watch(&self, options: WatchOptions) -> Result<i32> {
        self.ensure_idle()?;
        let (products, mode) = match options {
            WatchOptions::LatestState { products } => (products, WatchMode::LatestState),
            WatchOptions::OrderedState {
                products,
                max_pending,
            } => (products, WatchMode::OrderedState { max_pending }),
        };
        self.slot.write(|slot| match &mut slot.session {
            Some(session) => session.open_watch(products, mode),
            None => slot.initial_watches.prepare_watch(products, mode),
        })
    }

    pub fn next_watch(
        &self,
        watch: i32,
        reserve: impl FnOnce() -> Result<()>,
    ) -> Result<Option<WatchUpdate>> {
        self.ensure_idle()?;
        self.slot.write(|slot| match &mut slot.session {
            Some(session) => session.next_watch_with_reservation(watch, reserve),
            None => {
                ensure!(
                    slot.initial_watches.contains(watch),
                    "invalid or closed watch"
                );
                Ok(None)
            }
        })
    }

    pub fn replay_watch(&self, watch: i32) -> Result<bool> {
        self.ensure_idle()?;
        self.slot.write(|slot| match &mut slot.session {
            Some(session) => session.replay_watch(watch),
            None => {
                ensure!(
                    slot.initial_watches.contains(watch),
                    "invalid or closed watch"
                );
                Ok(false)
            }
        })
    }

    pub fn close_watch(&self, watch: i32) -> Result<bool> {
        self.ensure_idle()?;
        Ok(self.slot.write(|slot| match &mut slot.session {
            Some(session) => session.close_watch(watch),
            None => slot.initial_watches.close_watch(watch),
        }))
    }

    pub fn finish_watch(
        &self,
        watch: i32,
        ticket: i32,
        outcome: WatchOutcome<'_>,
        decisions: &[WatchDecision<'_>],
    ) -> Result<usize> {
        self.ensure_idle()?;
        self.slot.write(|slot| {
            slot.session
                .as_mut()
                .context("session is not initialized")?
                .finish_watch(watch, ticket, outcome, decisions)
        })
    }
}
