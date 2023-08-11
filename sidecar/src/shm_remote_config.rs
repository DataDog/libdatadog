// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::one_way_shared_memory::{open_named_shm, OneWayShmReader, OneWayShmWriter, ReaderOpener};
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use base64::Engine;
use datadog_ipc::platform::{FileBackedHandle, MappedMem, NamedShmHandle};
use datadog_remote_config::{RemoteConfigPath, RemoteConfigValue, Target};
use datadog_remote_config::fetch::{ConfigInvariants, FileRefcountData, FileStorage, MultiTargetFetcher, MultiTargetHandlers, NotifyTarget, RefcountedFile};
use std::cmp::Reverse;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::default::Default;
use std::ffi::CString;
use std::hash::{Hash, Hasher};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use priority_queue::PriorityQueue;
use tokio::time::Instant;
use tracing::{debug, error, trace, warn};
use zwohash::{HashSet, ZwoHasher};
use crate::primary_sidecar_identifier;

pub struct RemoteConfigWriter(OneWayShmWriter<NamedShmHandle>);
pub struct RemoteConfigReader(OneWayShmReader<NamedShmHandle, CString>);

fn path_for_remote_config(id: &ConfigInvariants, target: &Arc<Target>) -> CString {
    // We need a stable hash so that the outcome is independent of the process
    let mut hasher = ZwoHasher::default();
    id.hash(&mut hasher);
    target.hash(&mut hasher);
    CString::new(format!("/libdatadog-remote-config-{}-{}", primary_sidecar_identifier(), hasher.finish())).unwrap()
}

impl RemoteConfigReader {
    pub fn new(id: &ConfigInvariants, target: &Arc<Target>) -> RemoteConfigReader {
        let path = path_for_remote_config(id, target);
        RemoteConfigReader(OneWayShmReader::new(
            open_named_shm(&path).ok(),
            path,
        ))
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

impl ReaderOpener<NamedShmHandle>
    for OneWayShmReader<NamedShmHandle, CString>
{
    fn open(&self) -> Option<MappedMem<NamedShmHandle>> {
        open_named_shm(&self.extra).ok()
    }
}

#[derive(Clone)]
struct ConfigFileStorage {
    invariants: ConfigInvariants,
    /// All writers
    writers: Arc<Mutex<HashMap<Arc<Target>, RemoteConfigWriter>>>,
    on_dead: Arc<Box<dyn Fn() + Sync + Send>>,
}

struct StoredShmFile {
    handle: Mutex<NamedShmHandle>,
    refcount: FileRefcountData,
}

impl RefcountedFile for StoredShmFile {
    fn refcount(&self) -> &FileRefcountData {
        &self.refcount
    }
}

impl FileStorage for ConfigFileStorage {
    type StoredFile = StoredShmFile;

    fn store(&self, version: u64, path: RemoteConfigPath, file: Vec<u8>) -> anyhow::Result<Arc<StoredShmFile>> {
        Ok(Arc::new(StoredShmFile {
            handle: Mutex::new(store_shm(version, &path, file)?),
            refcount: FileRefcountData::new(version, path),
        }))
    }

    fn update(&self, file: &Arc<Self::StoredFile>, version: u64, contents: Vec<u8>) -> anyhow::Result<()> {
        *file.handle.lock().unwrap() = store_shm(version, &file.refcount.path, contents)?;
        Ok(())
    }
}

fn store_shm(version: u64, path: &RemoteConfigPath, file: Vec<u8>) -> io::Result<NamedShmHandle> {
    let name = format!(
        "/libdatadog-remote-config-file-{}-{}-{}",
        primary_sidecar_identifier(),
        version,
        BASE64_URL_SAFE_NO_PAD.encode(path.to_string())
    );
    let mut handle =
        NamedShmHandle::create(CString::new(name)?, file.len())?
            .map()?;

    handle.as_slice_mut().copy_from_slice(file.as_slice());

    Ok(handle.into())
}

impl MultiTargetHandlers<StoredShmFile> for ConfigFileStorage {
    fn fetched(&self, target: &Arc<Target>, files: &[Arc<StoredShmFile>]) -> (Option<String>, bool) {
        let mut writers = self.writers.lock().unwrap();
        let writer = match writers.entry(target.clone()) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(match RemoteConfigWriter::new(&self.invariants, target) {
                Ok(w) => w,
                Err(e) => {
                    let msg = format!("Failed acquiring a remote config shm writer: {:?}", e);
                    error!(msg);
                    return (Some(msg), false);
                },
            }),
        };

        let len = files.iter().map(|f| f.handle.lock().unwrap().get_path().len() + 2).sum();
        let mut serialized = Vec::with_capacity(len);
        for file in files.iter() {
            serialized.extend_from_slice(file.handle.lock().unwrap().get_path());
            serialized.push(b'\n');
        }

        if writer.0.as_slice() != serialized {
            writer.write(&serialized);

            debug!("Active configuration files are: {}", String::from_utf8_lossy(&serialized));

            (None, true)
        } else {
            (None, false)
        }
    }

    fn expired(&self, target: &Arc<Target>) {
        if let Some(writer) = self.writers.lock().unwrap().remove(target) {
            // clear to signal it's no longer being fetched
            writer.write(&[]);
        }
    }

    fn dead(&self) {
        (self.on_dead)();
    }
}

pub struct ShmRemoteConfigsGuard<N: NotifyTarget + 'static> {
    target: Arc<Target>,
    runtime_id: String,
    remote_configs: ShmRemoteConfigs<N>,
}

impl<N: NotifyTarget + 'static> Drop for ShmRemoteConfigsGuard<N> {
    fn drop(&mut self) {
        self.remote_configs.0.delete_runtime(&self.runtime_id, &self.target);
    }
}

#[derive(Clone)]
pub struct ShmRemoteConfigs<N: NotifyTarget + 'static>(Arc<MultiTargetFetcher<N, ConfigFileStorage>>);

// we collect services per env, so that we always query, for each runtime + env, all the services
// adding runtimes increases amount of services, removing services after a while

// one request per (runtime_id, RemoteConfigIdentifier) tuple: extra_services are all services pertaining to that env
// refcounting RemoteConfigIdentifier tuples by their unique runtime_id

impl<N: NotifyTarget + 'static> ShmRemoteConfigs<N> {
    pub fn new(invariants: ConfigInvariants, on_dead: Box<dyn Fn() + Sync + Send>) -> Self {
        let storage = ConfigFileStorage {
            invariants: invariants.clone(),
            writers: Default::default(),
            on_dead: Arc::new(on_dead),
        };
        ShmRemoteConfigs(MultiTargetFetcher::new(storage, invariants))
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
        self.0.add_runtime(runtime_id.clone(), notify_target, &target);
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

fn read_config(path: &str) -> anyhow::Result<RemoteConfigValue> {
    let mapped = NamedShmHandle::open(&CString::new(path)?)?.map()?;

    if let Some(rc_path) = path.split('-').nth(6) {
        let rc_path = String::from_utf8(BASE64_URL_SAFE_NO_PAD.decode(rc_path)?)?;
        RemoteConfigValue::try_parse(&rc_path, mapped.as_slice())
    } else {
        anyhow::bail!("could not read config; {} has less than six dashes", path);
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
    active_configs: HashSet<String>,
    last_read_configs: Vec<String>,
    check_configs: Vec<String>,
}

pub enum RemoteConfigUpdate {
    None,
    Add(RemoteConfigValue),
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
        }
    }

    /// Polls one configuration change.
    /// Has to be polled repeatedly until None is returned.
    pub fn fetch_update(&mut self) -> RemoteConfigUpdate {
        if let Some(ref target) = self.active_target {
            let reader = self.active_reader.get_or_insert_with(|| RemoteConfigReader::new(&self.invariants, target));

            let (changed, data) = reader.read();
            if changed {
                'fetch_new: {
                    let mut configs = vec![];
                    if !data.is_empty() {
                        let mut i = 0;
                        let mut start = 0;
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
                    self.last_read_configs = configs;
                    self.check_configs = self.active_configs.iter().cloned().collect();
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
                self.active_configs.remove(&config);
                return RemoteConfigUpdate::Remove(RemoteConfigPath::try_parse(&config).unwrap());
            }
        }

        while let Some(config) = self.last_read_configs.pop() {
            if !self.active_configs.contains(&config) {
                match read_config(&config) {
                    Ok(parsed) => {
                        trace!("Adding remote config file {config}: {parsed:?}");
                        self.active_configs.insert(config);
                        return RemoteConfigUpdate::Add(parsed);
                    }
                    Err(e) => warn!("Failed reading remote config file {config}; skipping: {e:?}"),
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
                    if current_configs.is_empty() {
                        current_configs = self.active_configs.iter().cloned().collect();
                    } else {
                        let mut pending = self.active_configs.clone();
                        for config in current_configs {
                            pending.insert(config);
                        }
                        current_configs = pending.into_iter().collect();
                    }
                }
                self.encountered_targets.insert(old_target.clone(), (reader, current_configs));
                self.unexpired_targets.push(old_target, Reverse(Instant::now()));
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
        self.check_configs = self.active_configs.iter().cloned().collect();
    }

    /// Resets the currently active target. The next configuration change polls will emit Remove()
    /// for all current tracked active configurations.
    pub fn reset_target(&mut self) {
        self.set_target(None);
        self.check_configs = self.active_configs.iter().cloned().collect();
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
