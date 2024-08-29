// Unless explicitly stated otherwise all files in this repository are licensed under the Apache
// License Version 2.0. This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::one_way_shared_memory::{
    open_named_shm, OneWayShmReader, OneWayShmWriter, ReaderOpener,
};
use crate::primary_sidecar_identifier;
use crate::shm_limiters::ShmLimiter;
use crate::tracer::SHM_LIMITER;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use base64::Engine;
use datadog_ipc::platform::{FileBackedHandle, MappedMem, NamedShmHandle};
use datadog_remote_config::fetch::{
    ConfigInvariants, FileRefcountData, FileStorage, MultiTargetFetcher, MultiTargetHandlers,
    NotifyTarget, RefcountedFile,
};
use datadog_remote_config::{RemoteConfigPath, RemoteConfigProduct, RemoteConfigValue, Target};
use priority_queue::PriorityQueue;
use sha2::{Digest, Sha224};
use std::cmp::Reverse;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::default::Default;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::io;
#[cfg(windows)]
use std::io::Write;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, error, trace, warn};
use zwohash::ZwoHasher;

pub struct RemoteConfigWriter(OneWayShmWriter<NamedShmHandle>);
pub struct RemoteConfigReader(OneWayShmReader<NamedShmHandle, CString>);

fn path_for_remote_config(id: &ConfigInvariants, target: &Arc<Target>) -> CString {
    // We need a stable hash so that the outcome is independent of the process
    let mut hasher = ZwoHasher::default();
    id.hash(&mut hasher);
    target.hash(&mut hasher);
    // datadog remote config, on macos we're restricted to 31 chars
    CString::new(format!(
        "/ddrc{}-{}",
        primary_sidecar_identifier(),
        hasher.finish()
    ))
    .unwrap()
}

impl RemoteConfigReader {
    pub fn new(id: &ConfigInvariants, target: &Arc<Target>) -> RemoteConfigReader {
        let path = path_for_remote_config(id, target);
        RemoteConfigReader(OneWayShmReader::new(open_named_shm(&path).ok(), path))
    }

    pub fn read(&mut self) -> (bool, &[u8]) {
        self.0.read()
    }
}

impl RemoteConfigWriter {
    pub fn new(id: &ConfigInvariants, target: &Arc<Target>) -> io::Result<RemoteConfigWriter> {
        Ok(RemoteConfigWriter(OneWayShmWriter::<NamedShmHandle>::new(
            path_for_remote_config(id, target),
        )?))
    }

    pub fn write(&self, contents: &[u8]) {
        self.0.write(contents)
    }
}

impl ReaderOpener<NamedShmHandle> for OneWayShmReader<NamedShmHandle, CString> {
    fn open(&self) -> Option<MappedMem<NamedShmHandle>> {
        open_named_shm(&self.extra).ok()
    }
}

#[derive(Clone)]
struct ConfigFileStorage {
    invariants: ConfigInvariants,
    /// All writers
    writers: Arc<Mutex<HashMap<Arc<Target>, RemoteConfigWriter>>>,
    #[allow(clippy::type_complexity)]
    on_dead: Arc<Mutex<Option<Box<dyn FnOnce() + Sync + Send>>>>,
}

struct StoredShmFile {
    handle: Mutex<NamedShmHandle>,
    limiter: Option<ShmLimiter>,
    refcount: FileRefcountData,
}

impl RefcountedFile for StoredShmFile {
    fn refcount(&self) -> &FileRefcountData {
        &self.refcount
    }
}

impl FileStorage for ConfigFileStorage {
    type StoredFile = StoredShmFile;

    fn store(
        &self,
        version: u64,
        path: Arc<RemoteConfigPath>,
        file: Vec<u8>,
    ) -> anyhow::Result<Arc<StoredShmFile>> {
        Ok(Arc::new(StoredShmFile {
            handle: Mutex::new(store_shm(version, &path, file)?),
            limiter: if path.product == RemoteConfigProduct::LiveDebugger {
                Some(SHM_LIMITER.lock().unwrap().alloc())
            } else {
                None
            },
            refcount: FileRefcountData::new(version, path),
        }))
    }

    fn update(
        &self,
        file: &Arc<Self::StoredFile>,
        version: u64,
        contents: Vec<u8>,
    ) -> anyhow::Result<()> {
        *file.handle.lock().unwrap() = store_shm(version, &file.refcount.path, contents)?;
        Ok(())
    }
}

fn store_shm(
    version: u64,
    path: &RemoteConfigPath,
    file: Vec<u8>,
) -> anyhow::Result<NamedShmHandle> {
    let name = format!("ddrc{}-{}", primary_sidecar_identifier(), version,);
    // as much signal as possible to be collision free
    let hashed_path = BASE64_URL_SAFE_NO_PAD.encode(Sha224::digest(path.to_string()));
    #[cfg(target_os = "macos")]
    let sliced_path = &hashed_path[..30 - name.len()];
    #[cfg(not(target_os = "macos"))]
    let sliced_path = &hashed_path;
    let name = format!("/{}-{}", name, sliced_path);
    let len = file.len();
    #[cfg(windows)]
    let len = len + 4;
    let mut handle = NamedShmHandle::create(CString::new(name)?, len)?.map()?;

    let mut target_slice = handle.as_slice_mut();
    #[cfg(windows)]
    {
        target_slice.write_all(&(file.len() as u32).to_ne_bytes())?;
    }
    target_slice.copy_from_slice(file.as_slice());

    Ok(handle.into())
}

impl MultiTargetHandlers<StoredShmFile> for ConfigFileStorage {
    fn fetched(
        &self,
        runtime_id: &Arc<String>,
        target: &Arc<Target>,
        files: &[Arc<StoredShmFile>],
    ) -> bool {
        let mut writers = self.writers.lock().unwrap();
        let writer = match writers.entry(target.clone()) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(match RemoteConfigWriter::new(&self.invariants, target) {
                Ok(w) => w,
                Err(e) => {
                    let msg = format!("Failed acquiring a remote config shm writer: {:?}", e);
                    error!(msg);
                    return false;
                }
            }),
        };

        let mut serialized = vec![];
        serialized.extend_from_slice(runtime_id.as_bytes());
        serialized.push(b'\n');
        for file in files.iter() {
            serialized.extend_from_slice(file.handle.lock().unwrap().get_path());
            serialized.push(b':');
            if let Some(ref limiter) = file.limiter {
                serialized.extend_from_slice(limiter.index().to_string().as_bytes());
            } else {
                serialized.push(b'0');
            }
            serialized.push(b':');
            serialized.extend_from_slice(
                BASE64_URL_SAFE_NO_PAD
                    .encode(file.refcount.path.to_string())
                    .as_bytes(),
            );
            serialized.push(b'\n');
        }

        if writer.0.as_slice() != serialized {
            writer.write(&serialized);

            debug!(
                "Active configuration files are: {}",
                String::from_utf8_lossy(&serialized)
            );

            true
        } else {
            false
        }
    }

    fn expired(&self, target: &Arc<Target>) {
        if let Some(writer) = self.writers.lock().unwrap().remove(target) {
            // clear to signal it's no longer being fetched
            writer.write(&[]);
        }
    }

    fn dead(&self) {
        (self
            .on_dead
            .lock()
            .unwrap()
            .take()
            .expect("The MultiTargetHandler must not be used anymore once on_dead is called"))(
        );
    }
}

pub struct ShmRemoteConfigsGuard<N: NotifyTarget + 'static> {
    target: Arc<Target>,
    runtime_id: String,
    remote_configs: ShmRemoteConfigs<N>,
}

impl<N: NotifyTarget + 'static> Drop for ShmRemoteConfigsGuard<N> {
    fn drop(&mut self) {
        self.remote_configs
            .0
            .delete_runtime(&self.runtime_id, &self.target);
        if self
            .remote_configs
            .0
            .invariants()
            .endpoint
            .test_token
            .is_some()
            && self.remote_configs.0.active_runtimes() == 0
        {
            self.remote_configs.shutdown()
        }
    }
}

#[derive(Clone)]
pub struct ShmRemoteConfigs<N: NotifyTarget + 'static>(
    Arc<MultiTargetFetcher<N, ConfigFileStorage>>,
);

// we collect services per env, so that we always query, for each runtime + env, all the services
// adding runtimes increases amount of services, removing services after a while

// one request per (runtime_id, RemoteConfigIdentifier) tuple: extra_services are all services
// pertaining to that env refcounting RemoteConfigIdentifier tuples by their unique runtime_id

impl<N: NotifyTarget + 'static> ShmRemoteConfigs<N> {
    pub fn new(invariants: ConfigInvariants, on_dead: Box<dyn FnOnce() + Sync + Send>) -> Self {
        let is_test = invariants.endpoint.test_token.is_some();
        let storage = ConfigFileStorage {
            invariants: invariants.clone(),
            writers: Default::default(),
            on_dead: Arc::new(Mutex::new(Some(on_dead))),
        };
        let fetcher = MultiTargetFetcher::new(storage, invariants);
        if is_test {
            fetcher
                .remote_config_interval
                .store(10_000_000, Ordering::Relaxed);
        }
        ShmRemoteConfigs(fetcher)
    }

    pub fn is_dead(&self) -> bool {
        self.0.is_dead()
    }

    pub fn add_runtime(
        &self,
        runtime_id: String,
        notify_target: N,
        env: String,
        service: String,
        app_version: String,
    ) -> ShmRemoteConfigsGuard<N> {
        let target = Arc::new(Target {
            service,
            env,
            app_version,
        });
        self.0
            .add_runtime(runtime_id.clone(), notify_target, &target);
        ShmRemoteConfigsGuard {
            target,
            runtime_id,
            remote_configs: self.clone(),
        }
    }

    pub fn shutdown(&self) {
        self.0.shutdown();
    }
}

fn read_config(path: &str) -> anyhow::Result<(RemoteConfigValue, u32)> {
    if let [shm_path, limiter, rc_path] = &path.split(':').collect::<Vec<_>>()[..] {
        let mapped = NamedShmHandle::open(&CString::new(*shm_path)?)?.map()?;
        let rc_path = String::from_utf8(BASE64_URL_SAFE_NO_PAD.decode(rc_path)?)?;
        let data = mapped.as_slice();
        #[cfg(windows)]
        let data = &data[4..(4 + u32::from_ne_bytes((&data[0..4]).try_into()?) as usize)];
        Ok((
            RemoteConfigValue::try_parse(&rc_path, data)?,
            u32::from_str(limiter)?,
        ))
    } else {
        anyhow::bail!(
            "could not read config; {} does not have exactly one colon",
            path
        );
    }
}

/// Manages configs.
/// Returns changes to configurations.
/// Switching targets is supported; Remove and Add operations will be yielded upon the next
/// fetch_update() call according to the difference.
/// It is guaranteed that no two configurations sharing the same RemoteConfigPath are applied at
/// once. They will always be Remove()d first, then Add()ed again upon update.
pub struct RemoteConfigManager {
    invariants: ConfigInvariants,
    active_target: Option<Arc<Target>>,
    active_reader: Option<RemoteConfigReader>,
    encountered_targets: HashMap<Arc<Target>, (RemoteConfigReader, Vec<String>)>,
    unexpired_targets: PriorityQueue<Arc<Target>, Reverse<Instant>>,
    active_configs: HashMap<String, RemoteConfigPath>,
    last_read_configs: Vec<String>,
    check_configs: Vec<String>,
    pub current_runtime_id: String,
}

#[derive(Debug)]
pub enum RemoteConfigUpdate {
    None,
    Add {
        value: RemoteConfigValue,
        limiter_index: u32,
    },
    Remove(RemoteConfigPath),
}

impl RemoteConfigManager {
    pub fn new(invariants: ConfigInvariants) -> RemoteConfigManager {
        RemoteConfigManager {
            invariants,
            active_target: None,
            active_reader: None,
            encountered_targets: Default::default(),
            unexpired_targets: Default::default(),
            active_configs: Default::default(),
            last_read_configs: Default::default(),
            check_configs: vec![],
            current_runtime_id: "".to_string(),
        }
    }

    /// Polls one configuration change.
    /// Has to be polled repeatedly until None is returned.
    pub fn fetch_update(&mut self) -> RemoteConfigUpdate {
        if let Some(ref target) = self.active_target {
            let reader = self
                .active_reader
                .get_or_insert_with(|| RemoteConfigReader::new(&self.invariants, target));

            let (changed, data) = reader.read();
            if changed {
                'fetch_new: {
                    let mut configs = vec![];
                    let mut runtime_id: &[u8] = b"";
                    if !data.is_empty() {
                        let mut i = 0;
                        while i < data.len() {
                            if data[i] == b'\n' {
                                break;
                            }
                            i += 1;
                        }
                        runtime_id = &data[0..i];
                        i += 1;
                        let mut start = i;
                        while i < data.len() {
                            if data[i] == b'\n' {
                                match std::str::from_utf8(&data[start..i]) {
                                    Ok(s) => configs.push(s.to_string()),
                                    Err(e) => {
                                        warn!("Failed reading received configurations {e:?}");
                                        break 'fetch_new;
                                    }
                                }
                                start = i + 1;
                            }
                            i += 1;
                        }
                    }
                    match std::str::from_utf8(runtime_id) {
                        Ok(s) => self.current_runtime_id = s.to_string(),
                        Err(e) => {
                            warn!("Failed reading received configurations {e:?}");
                            break 'fetch_new;
                        }
                    }
                    self.last_read_configs = configs;
                    self.check_configs = self.active_configs.keys().cloned().collect();
                }

                while let Some((_, Reverse(instant))) = self.unexpired_targets.peek() {
                    if *instant < Instant::now() - Duration::from_secs(3666) {
                        let (target, _) = self.unexpired_targets.pop().unwrap();
                        self.encountered_targets.remove(&target);
                    }
                }
            }
        }

        while let Some(config) = self.check_configs.pop() {
            if !self.last_read_configs.contains(&config) {
                trace!("Removing remote config file {config}");
                if let Some(path) = self.active_configs.remove(&config) {
                    return RemoteConfigUpdate::Remove(path);
                }
            }
        }

        while let Some(config) = self.last_read_configs.pop() {
            if let Entry::Vacant(entry) = self.active_configs.entry(config) {
                match read_config(entry.key()) {
                    Ok((parsed, limiter_index)) => {
                        trace!("Adding remote config file {}: {:?}", entry.key(), parsed);
                        entry.insert(RemoteConfigPath {
                            source: parsed.source,
                            product: (&parsed.data).into(),
                            config_id: parsed.config_id.clone(),
                            name: parsed.name.clone(),
                        });
                        return RemoteConfigUpdate::Add {
                            value: parsed,
                            limiter_index,
                        };
                    }
                    Err(e) => warn!(
                        "Failed reading remote config file {}; skipping: {:?}",
                        entry.key(),
                        e
                    ),
                }
            }
        }

        RemoteConfigUpdate::None
    }

    fn set_target(&mut self, target: Option<Arc<Target>>) {
        let mut current_configs = std::mem::take(&mut self.last_read_configs);
        if let Some(old_target) = std::mem::replace(&mut self.active_target, target) {
            if let Some(reader) = self.active_reader.take() {
                // Reconstruct currently active configurations
                if self.check_configs.is_empty() {
                    current_configs.extend(self.active_configs.keys().cloned());
                }
                self.encountered_targets
                    .insert(old_target.clone(), (reader, current_configs));
                self.unexpired_targets
                    .push(old_target, Reverse(Instant::now()));
            }
        }
        if let Some(ref target) = self.active_target {
            if let Some((reader, last_fetch)) = self.encountered_targets.remove(target) {
                self.active_reader = Some(reader);
                self.last_read_configs = last_fetch;
                self.unexpired_targets.remove(target);
            }
        }
    }

    /// Sets the currently active target.
    pub fn track_target(&mut self, target: &Arc<Target>) {
        self.set_target(Some(target.clone()));
        self.check_configs = self.active_configs.keys().cloned().collect();
    }

    /// Resets the currently active target. The next configuration change polls will emit Remove()
    /// for all current tracked active configurations.
    pub fn reset_target(&mut self) {
        self.set_target(None);
        self.check_configs = self.active_configs.keys().cloned().collect();
    }

    pub fn get_target(&self) -> Option<&Arc<Target>> {
        self.active_target.as_ref()
    }

    /// Resets everything, giving up the target and all tracked state of active configurations.
    pub fn reset(&mut self) {
        self.set_target(None);
        self.check_configs.clear();
        self.active_configs.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_dynamic_configuration::{data::tests::dummy_dynamic_config, Configs};
    use datadog_remote_config::fetch::test_server::RemoteConfigServer;
    use datadog_remote_config::{RemoteConfigData, RemoteConfigProduct, RemoteConfigSource};
    use lazy_static::lazy_static;
    use manual_future::ManualFuture;

    lazy_static! {
        static ref PATH_FIRST: RemoteConfigPath = RemoteConfigPath {
            source: RemoteConfigSource::Employee,
            product: RemoteConfigProduct::ApmTracing,
            config_id: "1234".to_string(),
            name: "config".to_string(),
        };
        static ref PATH_SECOND: RemoteConfigPath = RemoteConfigPath {
            source: RemoteConfigSource::Employee,
            product: RemoteConfigProduct::ApmTracing,
            config_id: "9876".to_string(),
            name: "config".to_string(),
        };
        static ref DUMMY_TARGET: Arc<Target> = Arc::new(Target {
            service: "service".to_string(),
            env: "env".to_string(),
            app_version: "1.3.5".to_string(),
        });
    }

    #[derive(Debug, Clone)]
    struct NotifyDummy(Arc<tokio::sync::mpsc::Sender<()>>);

    impl Hash for NotifyDummy {
        fn hash<H: Hasher>(&self, _state: &mut H) {}
    }

    impl Eq for NotifyDummy {}

    impl PartialEq<Self> for NotifyDummy {
        fn eq(&self, _other: &Self) -> bool {
            true
        }
    }

    impl NotifyTarget for NotifyDummy {
        fn notify(&self) {
            let channel = self.0.clone();
            tokio::spawn(async move {
                channel.send(()).await.unwrap();
            });
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_shm_updates() {
        let server = RemoteConfigServer::spawn();

        let (on_dead, on_dead_completer) = ManualFuture::new();
        let shm = ShmRemoteConfigs::new(
            server.dummy_invariants(),
            Box::new(|| {
                tokio::spawn(on_dead_completer.complete(()));
            }),
        );

        let mut manager = RemoteConfigManager::new(server.dummy_invariants());

        server.files.lock().unwrap().insert(
            PATH_FIRST.clone(),
            (
                vec![DUMMY_TARGET.clone()],
                1,
                serde_json::to_string(&dummy_dynamic_config(true)).unwrap(),
            ),
        );

        // Nothing yet. (No target)
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));

        manager.track_target(&DUMMY_TARGET);
        // remote end has not fetched anything yet
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));

        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);

        let shm_guard = shm.add_runtime(
            "3b43524b-a70c-45dc-921d-34504e50c5eb".to_string(),
            NotifyDummy(Arc::new(sender)),
            DUMMY_TARGET.env.to_string(),
            DUMMY_TARGET.service.to_string(),
            DUMMY_TARGET.app_version.to_string(),
        );

        receiver.recv().await;

        if let RemoteConfigUpdate::Add { value, .. } = manager.fetch_update() {
            assert_eq!(value.config_id, PATH_FIRST.config_id);
            assert_eq!(value.source, PATH_FIRST.source);
            assert_eq!(value.name, PATH_FIRST.name);
            if let RemoteConfigData::DynamicConfig(data) = value.data {
                assert!(matches!(
                    <Vec<Configs>>::from(data.lib_config)[0],
                    Configs::TracingEnabled(true)
                ));
            } else {
                unreachable!();
            }
        } else {
            unreachable!();
        }

        // just one update
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));

        {
            let mut files = server.files.lock().unwrap();
            files.insert(
                PATH_FIRST.clone(),
                (
                    vec![DUMMY_TARGET.clone()],
                    2,
                    serde_json::to_string(&dummy_dynamic_config(false)).unwrap(),
                ),
            );
            files.insert(
                PATH_SECOND.clone(),
                (
                    vec![DUMMY_TARGET.clone()],
                    1,
                    serde_json::to_string(&dummy_dynamic_config(true)).unwrap(),
                ),
            );
        }

        receiver.recv().await;

        // files must be first removed; avoids (in practice) two concurring settings to overlap
        let x = manager.fetch_update();
        if let RemoteConfigUpdate::Remove(update) = x {
            assert_eq!(&update, &*PATH_FIRST);
        } else {
            unreachable!();
        }

        // then the adds
        let was_second = if let RemoteConfigUpdate::Add { value, .. } = manager.fetch_update() {
            value.config_id == PATH_SECOND.config_id
        } else {
            unreachable!();
        };
        if let RemoteConfigUpdate::Add { value, .. } = manager.fetch_update() {
            assert_eq!(
                &value.config_id,
                if was_second {
                    &PATH_FIRST.config_id
                } else {
                    &PATH_SECOND.config_id
                }
            );
        } else {
            unreachable!();
        };

        // And done
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));

        // Reset will keep old targets for a while in memory
        manager.reset_target();

        // and start to remove
        let was_second = if let RemoteConfigUpdate::Remove(update) = manager.fetch_update() {
            update == *PATH_SECOND
        } else {
            unreachable!();
        };

        manager.track_target(&DUMMY_TARGET);
        // If we re-track it's added again immediately
        if let RemoteConfigUpdate::Add { value, .. } = manager.fetch_update() {
            assert_eq!(
                &value.config_id,
                if was_second {
                    &PATH_SECOND.config_id
                } else {
                    &PATH_FIRST.config_id
                }
            );
        } else {
            unreachable!();
        };

        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));

        drop(shm_guard);
        shm.shutdown();

        on_dead.await;

        // After proper shutdown it must be like all configs were removed
        let was_second = if let RemoteConfigUpdate::Remove(update) = manager.fetch_update() {
            update == *PATH_SECOND
        } else {
            unreachable!();
        };
        if let RemoteConfigUpdate::Remove(update) = manager.fetch_update() {
            assert_eq!(
                &update,
                if was_second {
                    &*PATH_FIRST
                } else {
                    &*PATH_SECOND
                }
            );
        } else {
            unreachable!();
        };

        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));
    }
}
