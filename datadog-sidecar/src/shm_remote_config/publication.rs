// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! CDN is a writer-side input to the ordinary RC directory. Readers consume the
//! same directory and product parser as agent RC, with no second snapshot format.
//!
//! `PublicationStore` retains Agent products and forwards changes to each scoped
//! `Publication`, which combines them with the current CDN file. `CdnPublisher`
//! accepts CDN updates and registers notifications; `NotificationGuard` removes
//! a registration when its subscriber leaves.

use super::*;
use libdd_ffe::cdn::{ApplyError, CdnConfig, ConfigurationSink};
use libdd_ffe::rules_based::Configuration;
use serde::{Deserialize, Serialize};
use std::sync::Weak;

pub const CDN_PATH: &str = "datadog/0/FFE_FLAGS/cdn/config";

/// Asynchronous registration. The client can derive its RC directory before the
/// sidecar processes this message. Credentials select a separate publication.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CdnRegistration {
    /// Validated endpoint, client identity, and polling settings.
    pub config: CdnConfig,
    /// Agent source whose non-FFE products are included in this directory.
    pub agent_endpoint: libdd_common::Endpoint,
    /// The same application target used by the agent RC writer and this reader.
    pub target: Arc<Target>,
}

impl CdnRegistration {
    /// Derive the ordinary RC directory identity without contacting the sidecar.
    pub fn invariants(&self) -> io::Result<RemoteConfigInvariants> {
        let identity = serde_json::to_vec(&self.config).map_err(io::Error::other)?;
        Ok(RemoteConfigInvariants {
            publication_scope: Some(BASE64_URL_SAFE_NO_PAD.encode(Sha224::digest(identity))),
            config: ConfigInvariants {
                language: self.config.language.clone(),
                tracer_version: self.config.version.clone(),
                endpoint: self.agent_endpoint.clone(),
                agentless: None,
            },
        })
    }
}

type Key = (ConfigInvariants, Arc<Target>);
type Notify = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Sources {
    agent: Vec<u8>,
    // Retain the files referenced by the directory while any scoped publisher uses them.
    files: Vec<Arc<StoredShmFile>>,
    cdn: Vec<Weak<Mutex<Publication>>>,
}

/// Shared by the agent writer and CDN pollers within one sidecar server.
#[derive(Clone, Default)]
pub(crate) struct PublicationStore(Arc<Mutex<HashMap<Key, Sources>>>);

impl PublicationStore {
    pub(super) fn agent_changed(
        &self,
        invariants: &ConfigInvariants,
        target: &Arc<Target>,
        directory: &[u8],
        files: &[Arc<StoredShmFile>],
    ) {
        let mut sources = self.0.lock_or_panic();
        let source = sources
            .entry((invariants.clone(), target.clone()))
            .or_default();
        if directory.is_empty() && source.agent.is_empty() {
            return;
        }
        source.agent = directory.to_vec();
        source.files = files.to_vec();
        source.cdn.retain(|p| p.strong_count() != 0);
        for publication in source.cdn.iter().filter_map(Weak::upgrade) {
            let mut publication = publication.lock_or_panic();
            publication.agent = directory.to_vec();
            publication.agent_files = files.to_vec();
            if let Err(error) = publication.publish() {
                warn!("Could not publish combined remote configuration: {error}");
            }
        }
    }

    pub(super) fn expire_agent(&self, invariants: &ConfigInvariants, target: &Arc<Target>) {
        self.agent_changed(invariants, target, &[], &[]);
        self.0.lock_or_panic().retain(|_, source| {
            !source.agent.is_empty() || source.cdn.iter().any(|p| p.strong_count() != 0)
        });
    }

    pub(crate) fn cdn(&self, registration: &CdnRegistration) -> io::Result<CdnPublisher> {
        let invariants = registration.invariants()?;
        let base = invariants.config.clone();
        let mut sources = self.0.lock_or_panic();
        sources.retain(|_, source| {
            !source.agent.is_empty() || source.cdn.iter().any(|p| p.strong_count() != 0)
        });
        // A failed allocation must not leave an empty source retaining the target.
        let writer = RemoteConfigWriter::new(&invariants, &registration.target)?;
        let source = sources
            .entry((base, registration.target.clone()))
            .or_default();
        let publisher = Arc::new(Mutex::new(Publication {
            writer,
            invariants,
            target: registration.target.clone(),
            agent: source.agent.clone(),
            agent_files: source.files.clone(),
            configuration: Vec::new(),
            file: None,
            notifiers: HashMap::new(),
        }));
        source.cdn.push(Arc::downgrade(&publisher));
        // A source with no accepted configuration does not publish an empty snapshot.
        // Existing agent products can be delivered without waiting for CDN HTTP.
        if !source.agent.is_empty() {
            publisher.lock_or_panic().publish()?;
        }
        Ok(CdnPublisher(publisher))
    }
}

struct Publication {
    writer: RemoteConfigWriter,
    invariants: RemoteConfigInvariants,
    target: Arc<Target>,
    agent: Vec<u8>,
    agent_files: Vec<Arc<StoredShmFile>>,
    configuration: Vec<u8>,
    file: Option<NamedShmHandle>,
    notifiers: HashMap<u64, Notify>,
}

impl Publication {
    /// Replace Agent FFE entries with the current CDN file, preserving all other products.
    fn publish(&mut self) -> io::Result<()> {
        let mut lines = self.agent.split(|byte| *byte == b'\n');
        let mut directory = lines.next().unwrap_or_default().to_vec();
        directory.push(b'\n');
        for line in lines.filter(|line| !line.is_empty()) {
            let product = std::str::from_utf8(line)
                .ok()
                .and_then(|line| line.split(':').nth(2))
                .and_then(|path| BASE64_URL_SAFE_NO_PAD.decode(path).ok())
                .and_then(|path| String::from_utf8(path).ok())
                .and_then(|path| RemoteConfigPath::parse(&path).ok())
                .map(|path| path.product());
            if product != Some(RemoteConfigProduct::FfeFlags) {
                directory.extend_from_slice(line);
                directory.push(b'\n');
            }
        }
        if let Some(file) = &self.file {
            // SAFETY: the publication owns the handle, with no concurrent unlink.
            directory.extend_from_slice(unsafe { file.get_path() });
            directory.extend_from_slice(b":0:");
            directory.extend_from_slice(BASE64_URL_SAFE_NO_PAD.encode(CDN_PATH).as_bytes());
            directory.push(b'\n');
        }
        if publish_directory(&self.writer, &self.invariants, &self.target, &directory)? {
            for notify in self.notifiers.values() {
                notify();
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct CdnPublisher(Arc<Mutex<Publication>>);

impl CdnPublisher {
    pub(crate) fn register(&self, notify: Notify, generation: u64) -> NotificationGuard {
        let mut publication = self.0.lock_or_panic();
        let id = rand::random();
        if generation != u64::MAX
            && publication.writer.current_generation() != 0
            && publication.writer.current_generation() != generation
        {
            notify();
        }
        publication.notifiers.insert(id, notify);
        NotificationGuard {
            publisher: self.clone(),
            id,
        }
    }
}
impl ConfigurationSink for CdnPublisher {
    fn apply(&self, configuration: Configuration) -> Result<(), ApplyError> {
        let bytes = configuration.get_flags_configuration().ok_or(ApplyError)?;
        let mut publication = self.0.lock_or_panic();
        if publication.configuration == bytes.as_ref() {
            return Ok(());
        }
        let path = RemoteConfigPath::parse(CDN_PATH).map_err(|_| ApplyError)?;
        let file = store_shm(rand::random(), &path, bytes.to_vec()).map_err(|error| {
            warn!("Could not store CDN configuration in remote config shared memory: {error}");
            ApplyError
        })?;
        let old_file = publication.file.replace(file);
        if let Err(error) = publication.publish() {
            publication.file = old_file;
            warn!("Could not publish CDN remote configuration: {error}");
            return Err(ApplyError);
        }
        publication.configuration = bytes.into_owned();
        Ok(())
    }
    fn failed(&self) {} // Existing RC retains last good files through transport failures.
    fn confirmed(&self) {}
}

pub(crate) struct NotificationGuard {
    publisher: CdnPublisher,
    id: u64,
}

impl NotificationGuard {
    pub(crate) fn notify_current(&self, generation: u64) {
        let publication = self.publisher.0.lock_or_panic();
        if generation != u64::MAX
            && publication.writer.current_generation() != 0
            && publication.writer.current_generation() != generation
            && let Some(notify) = publication.notifiers.get(&self.id)
        {
            notify();
        }
    }
}

impl Drop for NotificationGuard {
    fn drop(&mut self) {
        self.publisher.0.lock_or_panic().notifiers.remove(&self.id);
    }
}

/// Both agent and CDN publication paths use the same write/dedup/callback rule.
pub(super) fn publish_directory(
    writer: &RemoteConfigWriter,
    invariants: &RemoteConfigInvariants,
    target: &Arc<Target>,
    bytes: &[u8],
) -> io::Result<bool> {
    if writer.writer.as_slice() == bytes {
        return Ok(false);
    }
    if !writer.write(bytes) {
        return Err(io::Error::other("remote config shared memory write failed"));
    }
    if let Some(notify) = unsafe { IN_PROC_NOTIFY_FUN } {
        notify(invariants, target);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_ffe::{cdn::CdnEndpoint, rules_based::UniversalFlagConfig};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn registration() -> CdnRegistration {
        CdnRegistration {
            config: CdnConfig {
                endpoint: CdnEndpoint::managed("datadoghq.com", "test", "key-one").unwrap(),
                language: "php".into(),
                version: "test".into(),
                poll_interval: Duration::from_secs(30),
                request_timeout: Duration::from_secs(1),
            },
            agent_endpoint: libdd_common::Endpoint::default(),
            target: Arc::new(Target::new(
                format!("cdn-{}", rand::random::<u64>()),
                "test".into(),
                "".into(),
                vec![],
                vec![],
            )),
        }
    }
    fn config(value: &str) -> Configuration {
        Configuration::from_server_response(UniversalFlagConfig::from_json(format!(
            r#"{{"createdAt":"2026-11-06T00:00:00Z","environment":{{"name":"{value}"}},"flags":{{}}}}"#).into_bytes()).unwrap())
    }
    fn manager(r: &CdnRegistration) -> RemoteConfigManager {
        let mut m = RemoteConfigManager::new_with_registry(
            r.invariants().unwrap(),
            Arc::new(default_registry().with::<UniversalFlagConfig>().unwrap()),
        );
        m.track_target(&r.target);
        m
    }
    fn assert_add(m: &mut RemoteConfigManager) {
        match m.fetch_update() {
            RemoteConfigUpdate::Add { value, .. } => assert_eq!(value.path.as_str(), CDN_PATH),
            _ => panic!("expected CDN Add"),
        }
        assert!(matches!(m.fetch_update(), RemoteConfigUpdate::None));
    }
    #[test]
    #[cfg_attr(miri, ignore = "requires native shared memory and timed waits")]
    fn ordinary_rc_waits_do_not_spin_with_or_without_a_source() {
        fn assert_waits(manager: &mut RemoteConfigManager) {
            // Consume any initial generation edge before checking the idle wait.
            manager.wait_for_change(Duration::ZERO);
            let started = std::time::Instant::now();
            for _ in 0..8 {
                manager.wait_for_change(Duration::from_millis(5));
            }
            assert!(started.elapsed() >= Duration::from_millis(20));
        }
        let r = registration();
        let mut manager = RemoteConfigManager::new(r.invariants().unwrap());
        assert_waits(&mut manager);
        manager.track_target(&r.target);
        assert_waits(&mut manager); // The directory is not available yet.
        let store = PublicationStore::default();
        let publisher = store.cdn(&r).unwrap();
        assert_waits(&mut manager); // The writer has no accepted snapshot yet.
        publisher.apply(config("ready")).unwrap();
        assert_waits(&mut manager);
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires native shared memory")]
    fn ordinary_rc_retains_flags_during_failure_and_writer_replacement() {
        let r = registration();
        let store = PublicationStore::default();
        let p = store.cdn(&r).unwrap();
        let mut m = manager(&r);
        p.apply(config("first")).unwrap();
        assert_add(&mut m);
        p.failed();
        assert!(matches!(m.fetch_update(), RemoteConfigUpdate::None));
        drop(p);
        let p = store.cdn(&r).unwrap();
        assert!(
            matches!(m.fetch_update(), RemoteConfigUpdate::None),
            "empty replacement cleared config"
        );
        p.apply(config("second")).unwrap();
        assert!(matches!(m.fetch_update(), RemoteConfigUpdate::Remove(_)));
        assert_add(&mut m);
    }
    #[test]
    #[cfg_attr(miri, ignore = "requires native shared memory")]
    fn credentials_select_another_directory_and_identical_bytes_do_not_notify() {
        let r = registration();
        let store = PublicationStore::default();
        let p = store.cdn(&r).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let _guard = p.register(
            Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }),
            0,
        );
        p.apply(config("first")).unwrap();
        p.apply(config("first")).unwrap();
        p.confirmed();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let count = calls.clone();
        let _second = p.register(
            Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }),
            0,
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "late subscriber was not notified"
        );
        let mut changed = r.clone();
        changed.config.endpoint = CdnEndpoint::managed("datadoghq.com", "test", "key-two").unwrap();
        assert_eq!(
            r.invariants().unwrap().config,
            changed.invariants().unwrap().config,
            "CDN identity must not change the Agent RC client's configuration"
        );
        assert_ne!(
            path_for_remote_config(&r.invariants().unwrap(), &r.target),
            path_for_remote_config(&changed.invariants().unwrap(), &changed.target)
        );
        let mut other = manager(&changed);
        assert!(matches!(other.fetch_update(), RemoteConfigUpdate::None));
    }
    #[test]
    #[cfg_attr(miri, ignore = "requires native shared memory")]
    fn writer_reconciles_agent_ffe_and_preserves_other_products() {
        let r = registration();
        let store = PublicationStore::default();
        let p = store.cdn(&r).unwrap();
        let base = r.invariants().unwrap().config;
        let agent_ffe = RemoteConfigPath::parse("datadog/0/FFE_FLAGS/agent/config").unwrap();
        let apm = RemoteConfigPath::parse("datadog/0/APM_TRACING/apm/config").unwrap();
        let ffe = store_shm(rand::random(), &agent_ffe, b"{}".to_vec()).unwrap();
        let other = store_shm(rand::random(), &apm, b"{}".to_vec()).unwrap();
        let mut directory = b"runtime\n".to_vec();
        for (path, file) in [(&agent_ffe, &ffe), (&apm, &other)] {
            directory.extend_from_slice(unsafe { file.get_path() });
            directory.extend_from_slice(b":0:");
            directory.extend_from_slice(BASE64_URL_SAFE_NO_PAD.encode(path.as_str()).as_bytes());
            directory.push(b'\n');
        }
        store.agent_changed(&base, &r.target, &directory, &[]);
        let mut initial = RemoteConfigReader::new(&r.invariants().unwrap(), &r.target);
        let (changed, data) = initial.read();
        assert!(
            changed,
            "unrelated agent products waited for a CDN response"
        );
        let text = std::str::from_utf8(data).unwrap();
        assert!(text.contains(&BASE64_URL_SAFE_NO_PAD.encode(apm.as_str())));
        assert!(!text.contains(&BASE64_URL_SAFE_NO_PAD.encode(agent_ffe.as_str())));
        p.apply(config("cdn")).unwrap();
        let mut reader = RemoteConfigReader::new(&r.invariants().unwrap(), &r.target);
        let (changed, data) = reader.read();
        assert!(changed);
        let text = std::str::from_utf8(data).unwrap();
        assert!(text.contains(&BASE64_URL_SAFE_NO_PAD.encode(apm.as_str())));
        assert!(text.contains(&BASE64_URL_SAFE_NO_PAD.encode(CDN_PATH)));
        assert!(!text.contains(&BASE64_URL_SAFE_NO_PAD.encode(agent_ffe.as_str())));
        // The normal reader never has a reconciliation filter that unload can bypass.
        let mut m = manager(&r);
        let _ = m.fetch_update();
        m.unload_configs(&[RemoteConfigProduct::FfeFlags]);
        while let RemoteConfigUpdate::Add { value, .. } = m.fetch_update() {
            assert_ne!(value.path, agent_ffe);
        }
        store.expire_agent(&base, &r.target);
        let mut after_expiry = RemoteConfigReader::new(&r.invariants().unwrap(), &r.target);
        let (_, directory) = after_expiry.read();
        let text = std::str::from_utf8(directory).unwrap();
        assert!(
            text.contains(&BASE64_URL_SAFE_NO_PAD.encode(CDN_PATH)),
            "Agent expiry removed an accepted CDN file"
        );
        assert!(
            !text.contains(&BASE64_URL_SAFE_NO_PAD.encode(apm.as_str())),
            "Agent expiry retained withdrawn Agent products"
        );
    }
    #[test]
    #[cfg_attr(miri, ignore = "requires native shared memory")]
    fn retained_agent_file_is_parseable_after_caller_releases_it() {
        use libdd_remote_config::config::dynamic::DynamicConfigFile;
        let r = registration();
        let store = PublicationStore::default();
        let base = r.invariants().unwrap().config;
        let path = Arc::new(RemoteConfigPath::parse("datadog/0/APM_TRACING/apm/config").unwrap());
        let handle = store_shm(
            1,
            &path,
            br#"{"action":"enable","lib_config":{"tracing_enabled":true}}"#.to_vec(),
        )
        .unwrap();
        // SAFETY: the live handle owns the path during construction.
        let directory = format!(
            "runtime\n{}:0:{}\n",
            unsafe { std::str::from_utf8(handle.get_path()).unwrap() },
            BASE64_URL_SAFE_NO_PAD.encode(path.as_str())
        );
        let file = Arc::new(StoredShmFile {
            handle: Mutex::new(Some(handle)),
            limiter: Mutex::new(None),
            refcount: FileRefcountData::new(1, path.clone()),
        });
        let weak = Arc::downgrade(&file);
        store.agent_changed(&base, &r.target, directory.as_bytes(), &[file]);
        // Only the source store owns the file now. A late CDN publisher must
        // retain it too, and an ordinary manager must still open and parse it.
        assert!(weak.upgrade().is_some());
        let publisher = store.cdn(&r).unwrap();
        publisher.apply(config("cdn")).unwrap();
        let mut reader = manager(&r);
        let mut saw_agent = false;
        let mut saw_cdn = false;
        while let RemoteConfigUpdate::Add { value, .. } = reader.fetch_update() {
            if value.path == *path {
                let data = value.data.unwrap();
                let parsed = data.downcast::<DynamicConfigFile>().unwrap();
                assert_eq!(parsed.action, "enable");
                assert!(matches!(
                    Vec::<Configs>::from(parsed.lib_config.clone()).as_slice(),
                    [Configs::TracingEnabled(true)]
                ));
                saw_agent = true;
            } else {
                assert_eq!(value.path.as_str(), CDN_PATH);
                saw_cdn = true;
            }
        }
        assert!(
            saw_agent && saw_cdn,
            "both retained products must be readable"
        );
        store.expire_agent(&base, &r.target);
        assert!(
            weak.upgrade().is_none(),
            "withdrawn Agent file was not released"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires native shared memory")]
    fn crucible_c07_expiring_absent_agent_keeps_retained_cdn() {
        let r = registration();
        let store = PublicationStore::default();
        let p = store.cdn(&r).unwrap();
        let mut m = manager(&r);
        p.apply(config("first")).unwrap();
        assert_add(&mut m);
        drop(p);
        let _replacement = store.cdn(&r).unwrap();
        assert!(matches!(m.fetch_update(), RemoteConfigUpdate::None));
        let base = r.invariants().unwrap().config;
        store.expire_agent(&base, &r.target);
        assert!(
            matches!(m.fetch_update(), RemoteConfigUpdate::None),
            "expiry without an accepted Agent snapshot removed retained CDN flags"
        );
        store.agent_changed(&base, &r.target, b"accepted-agent\n", &[]);
        assert!(
            matches!(m.fetch_update(), RemoteConfigUpdate::Remove(_)),
            "an accepted empty Agent directory must retain normal authoritative replacement"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "requires native shared memory")]
    fn crucible_c11_late_notification_respects_sentinel() {
        let r = registration();
        let store = PublicationStore::default();
        let p = store.cdn(&r).unwrap();
        p.apply(config("first")).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let guard = p.register(
            Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }),
            u64::MAX,
        );
        let after_register = calls.load(Ordering::SeqCst);
        guard.notify_current(u64::MAX);
        let after_notify_current = calls.load(Ordering::SeqCst);
        p.apply(config("changed")).unwrap();
        let after_change = calls.load(Ordering::SeqCst);
        eprintln!(
            "register={after_register}, notify_current={after_notify_current}, changed={after_change}"
        );
        assert_eq!(
            after_register, 0,
            "late registration ignored no-notify sentinel"
        );
        assert_eq!(
            after_notify_current, 0,
            "late app update ignored no-notify sentinel"
        );
        assert_eq!(after_change, 1, "future publications must still notify");
    }
}
