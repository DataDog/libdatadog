// Unless explicitly stated otherwise all files in this repository are licensed under the Apache
// License Version 2.0. This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use crate::service::{DynamicInstrumentationConfigState, InstanceId};
use crate::shm_path::keyed_shm_path;
use base64::Engine;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use libdd_capabilities_impl::{HttpClientCapability, NativeCapabilities};
use libdd_common::{MutexExt, tag::Tag};
use libdd_ipc::one_way_shared_memory::{OneWayShmReader, OneWayShmWriter, open_named_shm};
use libdd_ipc::platform::{FileBackedHandle, NamedShmHandle};
use libdd_ipc::rate_limiter::{ShmLimiter, ShmLimiterMemory};
use libdd_live_debugger::LiveDebuggingData;
use libdd_remote_config::config::dynamic::{Configs, parse_json};
use libdd_remote_config::fetch::{
    ConfigInvariants, FileRefcountData, FileStorage, MultiTargetFetcher, MultiTargetHandlers,
    MultiTargetStats, NotifyTarget, ProductCapabilities, RefcountedFile,
};
use libdd_remote_config::{
    ParserRegistry, RemoteConfigPath, RemoteConfigProduct, RemoteConfigValue, Target,
    default_registry,
};
use priority_queue::PriorityQueue;
use sha2::{Digest, Sha224};
use std::cmp::Reverse;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::default::Default;
use std::ffi::{CStr, CString};
use std::io;
#[cfg(windows)]
use std::io::Write;
use std::marker::PhantomData;
use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, error, trace, warn};

pub struct RemoteConfigWriter {
    writer: OneWayShmWriter<NamedShmHandle>,
    /// The state of dynamic instrumentation as per apm_config.
    /// We have to do some config merging with the actual config on the extension side here, as we
    /// may not do so on the extension side, due to the lifetime mismatch of both sides. (PHP
    /// request times may be in millisecond range, far too short for the extension to react and
    /// propagate information about the config.) We also make use of the fact that the multitarget
    /// fetcher only applies changes to the fetched config when any runtime is active, avoiding
    /// unnecessary flips of the products.
    dynamic_instrumentation: Option<bool>,
}
pub struct RemoteConfigReader(OneWayShmReader<NamedShmHandle, CString>);

/// Function to dump the invariants and target, and the corresponding path.
/// This is useful for debugging purposes.
///
/// # Safety
/// Pointers should be valid and non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn debug_dump_inv_tar(
    id: *const ConfigInvariants,
    target: *const Arc<Target>,
) {
    unsafe {
        let id = &*id;
        let target = &*target;
        debug!("ConfigInvariants: {:#?}", id);
        debug!("Target: {:#?}", target);
        debug!(
            "Shared memory path: {:?}",
            path_for_remote_config(id, target)
        );
    }
}

type InProcNotifyFn = extern "C" fn(*const ConfigInvariants, *const Arc<Target>);
static mut IN_PROC_NOTIFY_FUN: Option<InProcNotifyFn> = None;

/// # Safety
/// This function modifies a global without synchronization.
/// It is designed to be called by the main thread before other threads are spawned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_set_rc_notify_fn(notify_fn: Option<InProcNotifyFn>) {
    unsafe {
        IN_PROC_NOTIFY_FUN = notify_fn;
    }
}

pub fn path_for_remote_config(id: &ConfigInvariants, target: &Arc<Target>) -> CString {
    keyed_shm_path("ddrc", &(id, target))
}

impl RemoteConfigReader {
    pub fn new(id: &ConfigInvariants, target: &Arc<Target>) -> RemoteConfigReader {
        let path = path_for_remote_config(id, target);
        RemoteConfigReader(OneWayShmReader::new_with_opener(
            open_named_shm(&path).ok(),
            path,
            |path| open_named_shm(path).ok(),
        ))
    }

    pub fn from_path(path: &CStr) -> Self {
        #[allow(clippy::unwrap_used)]
        RemoteConfigReader(OneWayShmReader::new_with_opener(
            open_named_shm(path).ok(),
            CString::new(path.to_bytes()).unwrap(),
            |path| open_named_shm(path).ok(),
        ))
    }

    pub fn get_path(&self) -> &CStr {
        &self.0.extra
    }

    pub fn read(&mut self) -> (bool, &[u8]) {
        self.0.read()
    }

    fn reconnect(&mut self, path: CString) {
        if self.0.extra != path {
            self.0.reconnect(&path);
            self.0.extra = path;
        }
    }

    /// See [`OneWayShmReader::take_replaced`].
    pub fn take_replaced(&mut self) -> bool {
        self.0.take_replaced()
    }
}

impl RemoteConfigWriter {
    pub fn new(id: &ConfigInvariants, target: &Arc<Target>) -> io::Result<RemoteConfigWriter> {
        Ok(RemoteConfigWriter {
            writer: OneWayShmWriter::<NamedShmHandle>::new(path_for_remote_config(id, target))?,
            dynamic_instrumentation: None,
        })
    }

    /// Returns `false` if the segment could not be grown to hold `contents`, in which case
    /// nothing was published and the previous payload stays current.
    pub fn write(&self, contents: &[u8]) -> bool {
        self.writer.write(contents)
    }

    pub fn current_generation(&self) -> u64 {
        self.writer.current_generation()
    }
}

#[derive(Default)]
struct TargetInfo {
    preferred_dynamic_instrumentation: u32,
    allowed_dynamic_instrumentation: u32,
}

#[derive(Clone)]
struct ConfigFileStorage<N: NotifyTarget + 'static> {
    invariants: ConfigInvariants,
    limiter: Option<Arc<Mutex<ShmLimiterMemory<()>>>>,
    /// All writers
    writers: Arc<Mutex<HashMap<Arc<Target>, RemoteConfigWriter>>>,
    targets: Arc<Mutex<HashMap<Arc<Target>, TargetInfo>>>,
    #[allow(clippy::type_complexity)]
    on_dead: Arc<Mutex<Option<Box<dyn Fn() + Sync + Send>>>>,
    _phantom: PhantomData<N>,
}

struct StoredShmFile {
    handle: Mutex<Option<NamedShmHandle>>, // just an option to use the handle under lock
    limiter: Option<ShmLimiter<()>>,
    refcount: FileRefcountData,
}

impl RefcountedFile for StoredShmFile {
    fn refcount(&self) -> &FileRefcountData {
        &self.refcount
    }
}

impl<N: NotifyTarget + 'static> FileStorage for ConfigFileStorage<N> {
    type StoredFile = StoredShmFile;

    fn store(
        &self,
        version: u64,
        path: Arc<RemoteConfigPath>,
        file: Vec<u8>,
    ) -> anyhow::Result<Arc<StoredShmFile>> {
        Ok(Arc::new(StoredShmFile {
            handle: Mutex::new(Some(store_shm(version, &path, file)?)),
            // No limiter means no rate limiting: the segment could not be created, which is
            // already reported where it happened.
            limiter: if path.product() == RemoteConfigProduct::LiveDebugging {
                self.limiter.as_ref().and_then(|limiter| {
                    let allocated = limiter.lock_or_panic().alloc();
                    if allocated.is_none() {
                        warn!(
                            "No rate limiter slot available for live debugging config {path}; \
                             not rate limited"
                        );
                    }
                    allocated
                })
            } else {
                None
            },
            refcount: FileRefcountData::new(version, path),
        }))
    }
}

fn store_shm(
    version: u64,
    path: &RemoteConfigPath,
    file: Vec<u8>,
) -> anyhow::Result<NamedShmHandle> {
    let name = segment_name(&crate::shm_namespace().to_string(), version, path);
    let len = file.len();
    #[cfg(windows)]
    let len = len + 4;
    let mut handle = NamedShmHandle::create(CString::new(name)?, len)?.map()?;

    #[allow(unused_mut)]
    let mut target_slice = handle.as_slice_mut();
    #[cfg(windows)]
    {
        target_slice.write_all(&(file.len() as u32).to_ne_bytes())?;
    }
    target_slice.copy_from_slice(file.as_slice());

    Ok(handle.into())
}

/// `/ddrc<namespace>-<base64url(SHA224(namespace, version, path))>`, cut to the 31 bytes macOS
/// allows for a shared memory name.
///
/// The version and namespace go into the digest rather than into the name in decimal, so that
/// a long version cannot crowd the hash out of a truncated name: the hash always keeps at least
/// 14 characters (84 bits) on unix. The name still changes whenever the version does, which
/// readers rely on to notice changed contents. Readers never parse it; they only open the
/// names listed by the sidecar.
fn segment_name(namespace: &str, version: u64, path: &RemoteConfigPath) -> String {
    let mut hasher = Sha224::new();
    hasher.update((namespace.len() as u64).to_le_bytes());
    hasher.update(namespace);
    hasher.update(version.to_le_bytes());
    hasher.update(path.to_string());
    let hash = BASE64_URL_SAFE_NO_PAD.encode(hasher.finalize());
    let mut name = format!("/ddrc{namespace}-{hash}");
    if cfg!(unix) {
        name.truncate(31);
    }
    name
}

fn dynamic_instrumentation_is_enabled(apm_config: Option<bool>, info: &TargetInfo) -> bool {
    match apm_config {
        Some(true) => {
            info.allowed_dynamic_instrumentation > 0 || info.preferred_dynamic_instrumentation > 0
        }
        None => info.preferred_dynamic_instrumentation > 0,
        Some(false) => false,
    }
}

impl<N: NotifyTarget + 'static> ConfigFileStorage<N> {
    /// Tell the in-process consumer (the AppSec helper) that the target's listing changed.
    ///
    /// Must be called without holding any of this storage's locks: the callback re-reads the
    /// listing and may do lengthy work, like rebuilding the WAF.
    fn notify_in_proc(&self, target: &Arc<Target>) {
        if let Some(in_proc_notify) = unsafe { IN_PROC_NOTIFY_FUN } {
            in_proc_notify(
                &self.invariants as *const ConfigInvariants,
                target as *const Arc<Target>,
            );
        }
    }
}

impl<N: NotifyTarget + 'static> MultiTargetHandlers<N, Self, NativeCapabilities>
    for ConfigFileStorage<N>
{
    fn fetched(
        &self,
        fetcher: &Arc<MultiTargetFetcher<N, Self, NativeCapabilities>>,
        runtime_id: &Arc<String>,
        target: &Arc<Target>,
        files: &[Arc<StoredShmFile>],
    ) -> bool {
        let mut writers = self.writers.lock_or_panic();
        let writer = match writers.entry(target.clone()) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(match RemoteConfigWriter::new(&self.invariants, target) {
                Ok(w) => w,
                Err(e) => {
                    let msg = format!("Failed acquiring a remote config shm writer: {e:?}");
                    error!(msg);
                    return false;
                }
            }),
        };

        let old_dynamic_instrumentation = writer.dynamic_instrumentation.take();

        let mut serialized = vec![];
        serialized.extend_from_slice(runtime_id.as_bytes());
        serialized.push(b'\n');
        for file in files.iter() {
            {
                // Scope the guard so it is released before the `ApmTracing` branch
                // below re-locks the same handle. Edition 2024 drops the temporary at
                // the end of the statement (before the borrowed path slice is used),
                // so bind it here explicitly instead.
                let handle = file.handle.lock_or_panic();
                // SAFETY: no concurrent unlink() on this handle.
                #[allow(clippy::unwrap_used)]
                let path = unsafe { handle.as_ref().unwrap().get_path() };
                serialized.extend_from_slice(path);
            }
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

            if file.refcount.path.product() == RemoteConfigProduct::ApmTracing {
                let mut handle = file.handle.lock_or_panic();
                #[allow(clippy::unwrap_used)]
                let shm = handle.take().unwrap();
                #[allow(clippy::unwrap_used)]
                // if mapping our own memory fails, it's already broken
                let mapped = shm.map().unwrap();
                if let Ok(config) = parse_json(mapped.as_slice()) {
                    let configs: Vec<Configs> = config.lib_config.into();
                    for config in configs {
                        if let Configs::DynamicInstrumentationEnabled(enabled) = config {
                            writer.dynamic_instrumentation = Some(enabled);
                        }
                    }
                }
                *handle = Some(mapped.into());
            }
        }

        if old_dynamic_instrumentation != writer.dynamic_instrumentation
            && let Some(info) = self.targets.lock_or_panic().get(target)
        {
            let was_enabled = dynamic_instrumentation_is_enabled(old_dynamic_instrumentation, info);
            let now_enabled =
                dynamic_instrumentation_is_enabled(writer.dynamic_instrumentation, info);
            if was_enabled && !now_enabled {
                fetcher.unforce_product(target, RemoteConfigProduct::LiveDebugging);
            } else if !was_enabled && now_enabled {
                fetcher.force_product(target, RemoteConfigProduct::LiveDebugging);
            }
        }

        if writer.writer.as_slice() == serialized {
            return false;
        }
        writer.write(&serialized);
        drop(writers);

        debug!(
            "Active configuration files are: {}",
            String::from_utf8_lossy(&serialized)
        );

        // The files still listed stay alive until this returns, so the callback may open them.
        self.notify_in_proc(target);
        true
    }

    fn expired(&self, target: &Arc<Target>) {
        let writer = self.writers.lock_or_panic().remove(target);
        if let Some(writer) = writer {
            // clear to signal it's no longer being fetched
            writer.write(&[]);
            // Keep the listing's name alive while the callback reads it.
            self.notify_in_proc(target);
        }
    }

    fn dead(&self) {
        #[allow(clippy::expect_used)]
        self.on_dead
            .lock_or_panic()
            .as_ref()
            .expect("The MultiTargetHandler must not be used anymore once on_dead is called")(
        );
    }
}

pub struct ShmRemoteConfigsGuard<N: NotifyTarget + 'static> {
    target: Arc<Target>,
    runtime_id: String,
    session_id: String,
    remote_configs: ShmRemoteConfigs<N>,
    dynamic_instrumentation_state: DynamicInstrumentationConfigState,
}

impl<N: NotifyTarget + 'static> Drop for ShmRemoteConfigsGuard<N> {
    fn drop(&mut self) {
        let fetcher = &self.remote_configs.0;
        fetcher.delete_runtime(&self.runtime_id, &self.session_id, &self.target);
        if fetcher.invariants().endpoint.test_token.is_some() && fetcher.active_runtimes() == 0 {
            self.remote_configs.shutdown()
        }

        if self.dynamic_instrumentation_state != DynamicInstrumentationConfigState::Disabled {
            let writers = fetcher.storage.storage.writers.lock_or_panic();
            if let Entry::Occupied(mut entry) = fetcher
                .storage
                .storage
                .targets
                .lock_or_panic()
                .entry(self.target.clone())
            {
                let remove = {
                    let info = entry.get_mut();
                    let apm_config_dynamic_instrumentation = writers
                        .get(&self.target)
                        .and_then(|c| c.dynamic_instrumentation);
                    let mut freshly_disabled = false;
                    if self.dynamic_instrumentation_state
                        == DynamicInstrumentationConfigState::Enabled
                    {
                        info.preferred_dynamic_instrumentation -= 1;
                        freshly_disabled = apm_config_dynamic_instrumentation.is_none()
                            && info.preferred_dynamic_instrumentation == 0;
                    } else {
                        info.allowed_dynamic_instrumentation -= 1;
                    }
                    let remove = info.preferred_dynamic_instrumentation == 0
                        && info.allowed_dynamic_instrumentation == 0;
                    if remove && apm_config_dynamic_instrumentation.is_some() {
                        freshly_disabled = true;
                    }
                    if Some(false) != apm_config_dynamic_instrumentation && freshly_disabled {
                        fetcher.unforce_product(&self.target, RemoteConfigProduct::LiveDebugging);
                    }
                    remove
                };
                if remove {
                    entry.remove();
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct ShmRemoteConfigs<N: NotifyTarget + 'static>(
    Arc<MultiTargetFetcher<N, ConfigFileStorage<N>, NativeCapabilities>>,
);

// we collect services per env, so that we always query, for each runtime + env, all the services
// adding runtimes increases amount of services, removing services after a while

// one request per (runtime_id, RemoteConfigIdentifier) tuple: extra_services are all services
// pertaining to that env refcounting RemoteConfigIdentifier tuples by their unique runtime_id

impl<N: NotifyTarget + 'static> ShmRemoteConfigs<N> {
    /// The on_dead arg will be called when a fetcher has no active runtimes.
    /// Beware: This function may be called at any time, e.g. another thread might be attempting to
    /// call add_runtime() right when no other runtime was left. Be careful with the locking here.
    /// Will not be called multiple times, unless this specific case was encountered.
    pub fn new(
        invariants: ConfigInvariants,
        on_dead: Box<dyn Fn() + Sync + Send>,
        interval: Duration,
        limiter: Option<Arc<Mutex<ShmLimiterMemory<()>>>>,
    ) -> Self {
        let storage = ConfigFileStorage {
            invariants: invariants.clone(),
            limiter,
            writers: Default::default(),
            targets: Arc::new(Mutex::new(Default::default())),
            on_dead: Arc::new(Mutex::new(Some(on_dead))),
            _phantom: Default::default(),
        };
        let fetcher =
            MultiTargetFetcher::new(storage, invariants, NativeCapabilities::new_periodic());
        fetcher
            .remote_config_interval
            .store(interval.as_nanos() as u64, Ordering::Relaxed);
        ShmRemoteConfigs(fetcher)
    }

    pub fn is_dead(&self) -> bool {
        self.0.is_dead()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_runtime(
        &self,
        instance_id: InstanceId,
        remote_config_generation: u64,
        notify_target: N,
        env: String,
        service: String,
        app_version: String,
        tags: Vec<Tag>,
        product_capabilities: ProductCapabilities,
        dynamic_instrumentation_state: DynamicInstrumentationConfigState,
        process_tags: Vec<Tag>,
    ) -> ShmRemoteConfigsGuard<N> {
        let target = Arc::new(Target::new(
            service,
            env,
            app_version,
            tags.iter().map(|t| t.to_string()).collect(),
            process_tags.iter().map(|t| t.to_string()).collect(),
        ));
        self.0.add_runtime(
            instance_id.session_id.clone(),
            instance_id.runtime_id.clone(),
            notify_target.clone(),
            &target,
            product_capabilities,
        );

        let writers = self.0.storage.storage.writers.lock_or_panic();
        if let Some(writer) = writers.get(&target)
            && writer.current_generation() > remote_config_generation
        {
            debug!(
                "Notify {:?} about newer remote config changes existing",
                notify_target
            );
            notify_target.notify();
        }
        if dynamic_instrumentation_state != DynamicInstrumentationConfigState::Disabled {
            let mut targets = self.0.storage.storage.targets.lock_or_panic();
            let info = targets
                .entry(target.clone())
                .or_insert(TargetInfo::default());
            if dynamic_instrumentation_state == DynamicInstrumentationConfigState::Enabled {
                info.preferred_dynamic_instrumentation += 1;
            } else {
                info.allowed_dynamic_instrumentation += 1;
            }
            let apm_config_dynamic_instrumentation =
                writers.get(&target).and_then(|c| c.dynamic_instrumentation);
            if Some(false) != apm_config_dynamic_instrumentation {
                let freshly_enabled = if apm_config_dynamic_instrumentation.is_none() {
                    info.preferred_dynamic_instrumentation == 1
                } else {
                    (info.preferred_dynamic_instrumentation == 0
                        && info.allowed_dynamic_instrumentation == 1)
                        || (info.preferred_dynamic_instrumentation == 1
                            && info.allowed_dynamic_instrumentation == 0)
                };
                if freshly_enabled {
                    self.0
                        .force_product(&target, RemoteConfigProduct::LiveDebugging);
                }
            }
        }

        ShmRemoteConfigsGuard {
            target,
            runtime_id: instance_id.runtime_id,
            session_id: instance_id.session_id,
            remote_configs: self.clone(),
            dynamic_instrumentation_state,
        }
    }

    pub fn shutdown(&self) {
        self.0.shutdown();
    }

    pub fn stats(&self) -> MultiTargetStats {
        self.0.stats()
    }
}

fn read_config(path: &str, registry: &ParserRegistry) -> anyhow::Result<(RemoteConfigValue, u32)> {
    if let [shm_path, limiter, rc_path] = &path.split(':').collect::<Vec<_>>()[..] {
        let mapped = NamedShmHandle::open(&CString::new(*shm_path)?)?.map()?;
        let rc_path = String::from_utf8(BASE64_URL_SAFE_NO_PAD.decode(rc_path)?)?;
        let data = mapped.as_slice();
        #[cfg(windows)]
        let data = &data[4..(4 + u32::from_ne_bytes((&data[0..4]).try_into()?) as usize)];
        Ok((
            RemoteConfigValue::try_parse(&rc_path, data, registry)?,
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
    registry: Arc<ParserRegistry>,
    active_target: Option<Arc<Target>>,
    pub active_reader: Option<RemoteConfigReader>,
    encountered_targets: HashMap<Arc<Target>, (RemoteConfigReader, Vec<String>)>,
    unexpired_targets: PriorityQueue<Arc<Target>, Reverse<Instant>>,
    active_configs: HashMap<String, RemoteConfigPath>,
    last_read_configs: Vec<String>,
    /// Listed configs of the active target that could not be read. They are retried on every
    /// poll until they can be, or until they are no longer listed; only the first failure warns.
    failed_configs: HashSet<String>,
    /// Set when a poll is complete, so that the next one retries `failed_configs` once.
    retry_failed: bool,
    check_configs: Vec<String>,
    /// Reapply every config after replacement: unchanged limiter indices refer to a new arena.
    reapply_all: bool,
    pub current_runtime_id: String,
    /// Runs right before a listed segment is opened, to race a writer against the reader.
    #[cfg(test)]
    before_open: Option<Box<dyn FnMut()>>,
}

#[allow(clippy::large_enum_variant)]
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
        #[allow(clippy::expect_used)]
        let registry = default_registry()
            .with::<LiveDebuggingData>()
            .expect("RemoteConfigManager::new: LiveDebugger is distinct from default products");
        Self::new_with_registry(invariants, Arc::new(registry))
    }

    pub fn new_with_registry(
        invariants: ConfigInvariants,
        registry: Arc<ParserRegistry>,
    ) -> RemoteConfigManager {
        RemoteConfigManager {
            invariants,
            registry,
            active_target: None,
            active_reader: None,
            encountered_targets: Default::default(),
            unexpired_targets: Default::default(),
            active_configs: Default::default(),
            last_read_configs: Default::default(),
            failed_configs: Default::default(),
            retry_failed: false,
            check_configs: vec![],
            reapply_all: false,
            current_runtime_id: "".to_string(),
            #[cfg(test)]
            before_open: None,
        }
    }

    /// Returns the generation last read by this client, or 0 if nothing has been read yet.
    pub fn current_remote_config_generation(&self) -> u64 {
        self.active_reader
            .as_ref()
            .map(|r| r.0.last_read_generation())
            .unwrap_or(0)
    }

    /// Rebind cached targets when reconnecting to another sidecar namespace.
    pub fn reconnect(&mut self) {
        for (target, (reader, _)) in &mut self.encountered_targets {
            reader.reconnect(path_for_remote_config(&self.invariants, target));
        }
        if let (Some(reader), Some(target)) = (&mut self.active_reader, &self.active_target) {
            reader.reconnect(path_for_remote_config(&self.invariants, target));
        }
    }

    /// The active directory's name in the connected sidecar's namespace.
    pub fn get_path(&self) -> Option<&CStr> {
        self.active_reader
            .as_ref()
            .map(RemoteConfigReader::get_path)
    }

    /// Polls one configuration change.
    /// Has to be polled repeatedly until None is returned.
    pub fn fetch_update(&mut self) -> RemoteConfigUpdate {
        if let Some(ref target) = self.active_target {
            if self.active_reader.is_none() {
                self.active_reader = Some(RemoteConfigReader::new(&self.invariants, target));
            }
            if self.refresh_listing() {
                let expiry = Instant::now().checked_sub(Duration::from_secs(3666));
                while let Some((_, Reverse(instant))) = self.unexpired_targets.peek() {
                    #[allow(clippy::unwrap_used)]
                    if expiry.is_some_and(|e| *instant < e) {
                        let (target, _) = self.unexpired_targets.pop().unwrap();
                        self.encountered_targets.remove(&target);
                    } else {
                        break;
                    }
                }
            }
        }

        while let Some(config) = self.check_configs.pop() {
            if self.reapply_all || !self.last_read_configs.contains(&config) {
                trace!("Removing remote config file {config}");
                if let Some(path) = self.active_configs.remove(&config) {
                    return RemoteConfigUpdate::Remove(path);
                }
            }
        }
        self.reapply_all = false;

        if std::mem::take(&mut self.retry_failed) {
            self.last_read_configs
                .extend(self.failed_configs.iter().cloned());
        }

        while let Some(config) = self.last_read_configs.pop() {
            if let Entry::Vacant(entry) = self.active_configs.entry(config) {
                #[cfg(test)]
                if let Some(hook) = self.before_open.as_mut() {
                    hook();
                }
                match read_config(entry.key(), &self.registry) {
                    Ok((parsed, limiter_index)) => {
                        trace!("Adding remote config file {}: {:?}", entry.key(), parsed);
                        self.failed_configs.remove(entry.key());
                        entry.insert(parsed.path.clone());
                        return RemoteConfigUpdate::Add {
                            value: parsed,
                            limiter_index,
                        };
                    }
                    Err(e) => {
                        let config = entry.into_key();
                        // The writer publishes a new listing before unlinking the segments the
                        // old one names, so a changed listing means this line is stale.
                        if self.refresh_listing() {
                            debug!(
                                "Remote config file {config} is gone with its listing; moving to \
                                 the current listing: {e:?}"
                            );
                            return self.fetch_update();
                        }
                        if self.failed_configs.insert(config.clone()) {
                            warn!(
                                "Failed reading remote config file {config}; retrying on later \
                                 polls: {e:?}"
                            );
                        } else {
                            debug!("Failed reading remote config file {config} again: {e:?}");
                        }
                    }
                }
            }
        }

        self.retry_failed = true;
        RemoteConfigUpdate::None
    }

    /// Reads the active target's listing again and, if it changed, makes it the one to apply:
    /// configs it no longer names are removed before any of its own configs is added.
    /// Returns whether it changed.
    fn refresh_listing(&mut self) -> bool {
        let Some(reader) = self.active_reader.as_mut() else {
            return false;
        };
        let (changed, data) = reader.read();
        if !changed {
            return false;
        }
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
            self.failed_configs.clear();
            self.check_configs = self.active_configs.keys().cloned().collect();
        }
        if self
            .active_reader
            .as_mut()
            .is_some_and(|r| r.take_replaced())
        {
            debug!("The remote config directory was replaced; re-applying all configs");
            self.check_configs = self.active_configs.keys().cloned().collect();
            self.reapply_all = true;
        }
        true
    }

    fn set_target(&mut self, target: Option<Arc<Target>>) {
        let mut current_configs = std::mem::take(&mut self.last_read_configs);
        if let Some(old_target) = std::mem::replace(&mut self.active_target, target)
            && let Some(reader) = self.active_reader.take()
        {
            // Reconstruct currently active configurations
            if self.check_configs.is_empty() {
                current_configs.extend(self.active_configs.keys().cloned());
            }
            // Still listed and not applied: retry them when the target is active again.
            current_configs.extend(self.failed_configs.drain());
            self.encountered_targets
                .insert(old_target.clone(), (reader, current_configs));
            self.unexpired_targets
                .push(old_target, Reverse(Instant::now()));
        }
        // Never retry another target's listing.
        self.failed_configs.clear();
        if let Some(ref target) = self.active_target
            && let Some((reader, last_fetch)) = self.encountered_targets.remove(target)
        {
            self.active_reader = Some(reader);
            self.last_read_configs = last_fetch;
            self.unexpired_targets.remove(target);
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
        self.reapply_all = false;
        // set_target() cached the active reader; discard it too.
        self.encountered_targets.clear();
        self.unexpired_targets.clear();
    }

    /// Can be used to fast-remove configs temporarily. Will be re-applied on next fetch_update().
    pub fn unload_configs(&mut self, configs: &[RemoteConfigProduct]) {
        self.active_configs.retain(|key, path| {
            if configs.contains(&path.product()) {
                // self.check_configs should generally be empty here, but be safe
                if let Some(pos) = self.check_configs.iter().position(|x| x == key) {
                    self.check_configs.swap_remove(pos);
                }
                // And re-apply it on next read
                self.last_read_configs.push(key.clone());
                false
            } else {
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_remote_config::config::dynamic::{
        Configs, DynamicConfigFile, tests::dummy_dynamic_config,
    };
    use libdd_remote_config::fetch::test_server::RemoteConfigServer;
    use manual_future::ManualFuture;
    use std::hash::{Hash, Hasher};
    use std::sync::LazyLock;

    static PATH_FIRST: LazyLock<RemoteConfigPath> = LazyLock::new(|| {
        RemoteConfigPath::parse("employee/APM_TRACING/1234/config").expect("valid path")
    });

    static PATH_SECOND: LazyLock<RemoteConfigPath> = LazyLock::new(|| {
        RemoteConfigPath::parse("employee/APM_TRACING/9876/config").expect("valid path")
    });

    static PATH_LIVE_DEBUGGER: LazyLock<RemoteConfigPath> = LazyLock::new(|| {
        RemoteConfigPath::parse("employee/LIVE_DEBUGGING/ld-1/config").expect("valid path")
    });

    static DUMMY_TARGET: LazyLock<Arc<Target>> = LazyLock::new(|| {
        Arc::new(Target::new(
            "service".to_string(),
            "env".to_string(),
            "1.3.5".to_string(),
            vec![],
            vec![],
        ))
    });

    #[test]
    fn shm_paths_distinguish_targets_and_endpoints() {
        let mut invariants = ConfigInvariants {
            language: "php".into(),
            tracer_version: "1.0".into(),
            endpoint: libdd_common::Endpoint::from_slice("http://agent-a:8126"),
            agentless: None,
        };
        let path = path_for_remote_config(&invariants, &DUMMY_TARGET);
        let target = Arc::new(Target::new(
            "other-service".into(),
            "env".into(),
            "1.3.5".into(),
            vec![],
            vec![],
        ));
        assert_ne!(path, path_for_remote_config(&invariants, &target));
        invariants.endpoint = libdd_common::Endpoint::from_slice("http://agent-b:8126");
        assert_ne!(path, path_for_remote_config(&invariants, &DUMMY_TARGET));
    }

    /// Every (namespace, version, path) gets a segment of its own, in a name macOS accepts.
    #[test]
    fn segment_names_are_unique_and_fit_macos() {
        // The longest namespace and version leave the least room for the path's hash.
        assert_ne!(
            segment_name("t4294967295", u64::MAX, &PATH_FIRST),
            segment_name("t4294967295", u64::MAX, &PATH_SECOND)
        );
        let paths = [&*PATH_FIRST, &*PATH_SECOND, &*PATH_LIVE_DEBUGGER];
        let mut names = std::collections::HashSet::new();
        for namespace in ["0", "4294967295", "t4294967295"] {
            for version in [0, 1, 2, 1_000_000_007, u64::MAX - 1, u64::MAX] {
                for path in paths {
                    let name = segment_name(namespace, version, path);
                    assert!(
                        names.insert(name.clone()),
                        "{name} is shared by two (namespace, version, path) triples"
                    );
                    if cfg!(unix) {
                        assert!(name.len() <= 31, "{name} is longer than 31 bytes");
                    }
                    assert!(name.starts_with("/ddrc"), "{name}");
                    assert!(!name[1..].contains('/'), "{name}");
                    assert_eq!(name, segment_name(namespace, version, path));
                }
            }
        }
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

    type WritersProbe = (
        ConfigInvariants,
        Arc<Mutex<HashMap<Arc<Target>, RemoteConfigWriter>>>,
        Vec<(bool, Vec<u8>)>,
    );
    static NOTIFY_PROBE: Mutex<Option<WritersProbe>> = Mutex::new(None);

    /// Records whether the writers were unlocked and what the listing said when notified.
    extern "C" fn probe_notify(invariants: *const ConfigInvariants, target: *const Arc<Target>) {
        let (invariants, target) = unsafe { (&*invariants, &*target) };
        let mut probe = NOTIFY_PROBE.lock().unwrap();
        if let Some((expected, writers, calls)) = probe.as_mut()
            && expected == invariants
        {
            let unlocked = writers.try_lock().is_ok();
            let mut reader = RemoteConfigReader::new(invariants, target);
            calls.push((unlocked, reader.read().1.to_vec()));
        }
    }

    /// The in-process callback (the AppSec helper) runs without the sidecar's locks held, and
    /// learns about a target's listing being cleared, not only about it being filled.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn in_process_notification_runs_unlocked_and_on_expiry() {
        let invariants = ConfigInvariants {
            language: "php".to_string(),
            tracer_version: "test".to_string(),
            // The file scheme disables background fetching.
            endpoint: libdd_common::Endpoint::from_slice(&format!(
                "file:///rc-notify-{}",
                std::process::id()
            )),
            agentless: None,
        };
        let sidecar = ShmRemoteConfigs::<NotifyDummy>::new(
            invariants.clone(),
            Box::new(|| {}),
            Duration::from_secs(60),
            None,
        );
        let storage = &sidecar.0.storage.storage;
        *NOTIFY_PROBE.lock().unwrap() = Some((invariants.clone(), storage.writers.clone(), vec![]));
        unsafe { ddog_set_rc_notify_fn(Some(probe_notify)) };

        assert!(storage.fetched(
            &sidecar.0,
            &Arc::new("runtime".to_string()),
            &DUMMY_TARGET,
            &[]
        ));
        storage.expired(&DUMMY_TARGET);

        let (_, _, calls) = NOTIFY_PROBE.lock().unwrap().take().unwrap();
        assert_eq!(
            calls,
            vec![(true, b"runtime\n".to_vec()), (true, vec![])],
            "expected one unlocked notification per listing change"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn new_client_opening_an_old_directory_is_notified_by_its_replacement() {
        let invariants = ConfigInvariants {
            language: "php".to_string(),
            tracer_version: "test".to_string(),
            // The file scheme disables background fetching for this registration test.
            endpoint: libdd_common::Endpoint::from_slice(&format!(
                "file:///rc-generation-{}",
                std::process::id()
            )),
            agentless: None,
        };
        let old = RemoteConfigWriter::new(&invariants, &DUMMY_TARGET).unwrap();
        for _ in 0..3 {
            assert!(old.write(b"old-runtime\n"));
        }

        let sidecar = ShmRemoteConfigs::new(
            invariants.clone(),
            Box::new(|| {}),
            Duration::from_secs(60),
            None,
        );
        let mut client = RemoteConfigManager::new(invariants.clone());
        client.track_target(&DUMMY_TARGET);
        assert!(matches!(client.fetch_update(), RemoteConfigUpdate::None));
        assert_eq!(client.current_runtime_id, "old-runtime");
        let old_generation = client.current_remote_config_generation();

        let new = RemoteConfigWriter::new(&invariants, &DUMMY_TARGET).unwrap();
        assert!(new.write(b"new-runtime\n"));
        sidecar
            .0
            .storage
            .storage
            .writers
            .lock_or_panic()
            .insert(DUMMY_TARGET.clone(), new);

        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let register = |generation| {
            sidecar.add_runtime(
                InstanceId::new("session", "runtime"),
                generation,
                NotifyDummy(Arc::new(sender.clone())),
                DUMMY_TARGET.env().to_string(),
                DUMMY_TARGET.service().to_string(),
                DUMMY_TARGET.app_version().to_string(),
                vec![],
                ProductCapabilities {
                    products: vec![],
                    capabilities: vec![],
                },
                DynamicInstrumentationConfigState::Disabled,
                vec![],
            )
        };
        let _registration = register(old_generation);
        tokio::time::timeout(Duration::from_millis(200), receiver.recv())
            .await
            .expect("the old directory generation must trigger a notification")
            .unwrap();
        assert!(matches!(client.fetch_update(), RemoteConfigUpdate::None));
        assert_eq!(client.current_runtime_id, "new-runtime");
        let generation = client.current_remote_config_generation();
        assert!(generation > old_generation);

        let _registration = register(generation);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), receiver.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_shm_updates() {
        let server = RemoteConfigServer::spawn();

        let (on_dead, on_dead_completer) = ManualFuture::new();
        let on_dead_completer = Arc::new(Mutex::new(Some(on_dead_completer)));
        let shm = ShmRemoteConfigs::new(
            server.dummy_options().invariants,
            Box::new(move || {
                if let Some(completer) = on_dead_completer.lock().unwrap().take() {
                    tokio::spawn(completer.complete(()));
                }
            }),
            Duration::from_millis(10),
            None,
        );

        let mut manager = RemoteConfigManager::new(server.dummy_options().invariants);

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
            InstanceId {
                session_id: "3b43524b-a70c-45dc-921d-34504e50c5eb".to_string(),
                runtime_id: "3b43524b-a70c-45dc-921d-34504e50c5eb".to_string(),
            },
            0,
            NotifyDummy(Arc::new(sender)),
            "env".to_string(),
            "service".to_string(),
            "1.3.5".to_string(),
            vec![],
            ProductCapabilities {
                products: server.dummy_options().products,
                capabilities: server.dummy_options().capabilities,
            },
            DynamicInstrumentationConfigState::Disabled,
            vec![],
        );

        receiver.recv().await;

        if let RemoteConfigUpdate::Add { value, .. } = manager.fetch_update() {
            assert_eq!(value.path.config_id(), PATH_FIRST.config_id());
            assert_eq!(value.path.source(), PATH_FIRST.source());
            assert_eq!(value.path.name(), PATH_FIRST.name());
            let parsed = value.data.as_ref().expect("dynamic config must parse");
            if let Some(cfg) = parsed.downcast::<DynamicConfigFile>() {
                assert!(matches!(
                    <Vec<Configs>>::from(cfg.lib_config.clone())[0],
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

        manager.unload_configs(&[PATH_FIRST.product()]);

        if let RemoteConfigUpdate::Add { value, .. } = manager.fetch_update() {
            assert_eq!(value.path.config_id(), PATH_FIRST.config_id());
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
            value.path.config_id() == PATH_SECOND.config_id()
        } else {
            unreachable!();
        };
        if let RemoteConfigUpdate::Add { value, .. } = manager.fetch_update() {
            assert_eq!(
                value.path.config_id(),
                if was_second {
                    PATH_FIRST.config_id()
                } else {
                    PATH_SECOND.config_id()
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
                value.path.config_id(),
                if was_second {
                    PATH_SECOND.config_id()
                } else {
                    PATH_FIRST.config_id()
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

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_live_debugger_config_parsed() {
        use libdd_live_debugger::LiveDebuggingData;

        let server = RemoteConfigServer::spawn();

        // The callback is mandatory but irrelevant here — we don't synchronize on teardown.
        let shm = ShmRemoteConfigs::new(
            server.dummy_options().invariants,
            Box::new(|| {}),
            Duration::from_millis(10),
            None,
        );

        let mut manager = RemoteConfigManager::new(server.dummy_options().invariants);

        // Minimal valid `ServiceConfiguration` payload — `LiveDebuggingData::ServiceConfiguration`
        // only requires `id` and `type` to parse.
        let live_debugger_json = r#"{"id":"ld-1","type":"SERVICE_CONFIGURATION"}"#;
        server.files.lock().unwrap().insert(
            PATH_LIVE_DEBUGGER.clone(),
            (
                vec![DUMMY_TARGET.clone()],
                1,
                live_debugger_json.to_string(),
            ),
        );

        manager.track_target(&DUMMY_TARGET);

        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let _shm_guard = shm.add_runtime(
            InstanceId {
                session_id: "3b43524b-a70c-45dc-921d-34504e50c5eb".to_string(),
                runtime_id: "3b43524b-a70c-45dc-921d-34504e50c5eb".to_string(),
            },
            0,
            NotifyDummy(Arc::new(sender)),
            "env".to_string(),
            "service".to_string(),
            "1.3.5".to_string(),
            vec![],
            ProductCapabilities {
                products: server.dummy_options().products,
                capabilities: server.dummy_options().capabilities,
            },
            DynamicInstrumentationConfigState::Enabled,
            vec![],
        );

        receiver.recv().await;

        if let RemoteConfigUpdate::Add { value, .. } = manager.fetch_update() {
            assert_eq!(value.path.config_id(), PATH_LIVE_DEBUGGER.config_id());
            assert_eq!(
                value.path.product(),
                RemoteConfigProduct::LiveDebugging,
                "must be parsed as LiveDebugger, not skipped"
            );
            let data = value.data.as_ref().expect("LiveDebugger must parse");
            let parsed = data
                .downcast::<LiveDebuggingData>()
                .expect("downcast to LiveDebuggingData should succeed");
            match parsed {
                LiveDebuggingData::ServiceConfiguration(sc) => assert_eq!(sc.id, "ld-1"),
                LiveDebuggingData::Probe(_) => unreachable!("expected ServiceConfiguration"),
            }
        } else {
            unreachable!("expected RemoteConfigUpdate::Add for the LiveDebugger config");
        }
    }

    /// A listed segment that cannot be opened yet is retried on a later poll, without the
    /// listing having to change and without re-adding the configs already applied.
    #[test]
    #[cfg_attr(miri, ignore)]
    // It warns, which test_logs_created_counter would count.
    #[serial_test::serial(log_counter)]
    fn a_listed_segment_failing_to_open_is_retried() {
        let invariants = ConfigInvariants {
            language: "php".to_string(),
            tracer_version: "test".to_string(),
            endpoint: libdd_common::Endpoint::from_slice(&format!(
                "file:///rc-retry-{}",
                std::process::id()
            )),
            agentless: None,
        };
        let path = |id: &str| {
            RemoteConfigPath::parse(&format!(
                "employee/APM_TRACING/{id}-{}/config",
                std::process::id()
            ))
            .unwrap()
        };
        let (late, present) = (path("late"), path("present"));
        let content = serde_json::to_vec(&dummy_dynamic_config(true)).unwrap();
        let present_file = store_shm(1, &present, content.clone()).unwrap();
        let late_name = segment_name(&crate::shm_namespace().to_string(), 1, &late);

        let mut listing = b"runtime\n".to_vec();
        for (name, path) in [
            (late_name.as_bytes(), &late),
            (unsafe { present_file.get_path() }, &present),
        ] {
            listing.extend_from_slice(name);
            listing.extend_from_slice(b":0:");
            listing.extend_from_slice(BASE64_URL_SAFE_NO_PAD.encode(path.to_string()).as_bytes());
            listing.push(b'\n');
        }
        let writer = RemoteConfigWriter::new(&invariants, &DUMMY_TARGET).unwrap();
        assert!(writer.write(&listing));

        let mut manager = RemoteConfigManager::new(invariants);
        manager.track_target(&DUMMY_TARGET);
        match manager.fetch_update() {
            RemoteConfigUpdate::Add { value, .. } => assert_eq!(value.path, present),
            other => panic!("expected the openable config to be added, got {other:?}"),
        }
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));
        // Still missing: nothing to add, and nothing removed.
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));

        let late_file = store_shm(1, &late, content).unwrap();
        assert_eq!(unsafe { late_file.get_path() }, late_name.as_bytes());
        match manager.fetch_update() {
            RemoteConfigUpdate::Add { value, .. } => assert_eq!(value.path, late),
            other => panic!("expected the late config to be added on retry, got {other:?}"),
        }
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));
    }

    /// A segment named by a listing that the writer has meanwhile replaced (and whose segments
    /// it has unlinked) is not a failure: the reader moves on to the current listing at once.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_segment_gone_with_its_listing_moves_to_the_current_listing() {
        use std::sync::atomic::AtomicUsize;
        use tracing_subscriber::layer::{Context, SubscriberExt};

        /// Counts this thread's warnings, leaving the global log counter alone.
        struct CountWarnings(Arc<AtomicUsize>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CountWarnings {
            fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
                if *event.metadata().level() == tracing::Level::WARN {
                    self.0.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        let warnings = Arc::new(AtomicUsize::new(0));
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::registry().with(CountWarnings(warnings.clone())),
        );
        let invariants = ConfigInvariants {
            language: "php".to_string(),
            tracer_version: "test".to_string(),
            endpoint: libdd_common::Endpoint::from_slice(&format!(
                "file:///rc-stale-{}",
                std::process::id()
            )),
            agentless: None,
        };
        let path = |id: &str| {
            RemoteConfigPath::parse(&format!(
                "employee/APM_TRACING/{id}-{}/config",
                std::process::id()
            ))
            .unwrap()
        };
        let (changing, stable) = (path("changing"), path("stable"));
        let content = serde_json::to_vec(&dummy_dynamic_config(true)).unwrap();
        let stable_file = store_shm(1, &stable, content.clone()).unwrap();
        // Version 1 is already unlinked; version 2 replaces it.
        let gone_name = segment_name(&crate::shm_namespace().to_string(), 1, &changing);
        let changed_file = store_shm(2, &changing, content).unwrap();
        let listing = |entries: [(&[u8], &RemoteConfigPath); 2]| {
            let mut listing = b"runtime\n".to_vec();
            for (name, path) in entries {
                listing.extend_from_slice(name);
                listing.extend_from_slice(b":0:");
                listing
                    .extend_from_slice(BASE64_URL_SAFE_NO_PAD.encode(path.to_string()).as_bytes());
                listing.push(b'\n');
            }
            listing
        };
        let stable_name = unsafe { stable_file.get_path() }.to_vec();
        let writer = Arc::new(RemoteConfigWriter::new(&invariants, &DUMMY_TARGET).unwrap());
        // The stable config is opened first, the gone one on the next call.
        assert!(writer.write(&listing([
            (gone_name.as_bytes(), &changing),
            (&stable_name, &stable)
        ])));

        let mut manager = RemoteConfigManager::new(invariants);
        manager.track_target(&DUMMY_TARGET);
        match manager.fetch_update() {
            RemoteConfigUpdate::Add { value, .. } => assert_eq!(value.path, stable),
            other => panic!("expected the stable config to be added, got {other:?}"),
        }

        // The writer publishes its next listing between the reader's read and its open.
        let next = listing([
            (unsafe { changed_file.get_path() }, &changing),
            (&stable_name, &stable),
        ]);
        let racing_writer = writer.clone();
        let mut publish = Some(move || assert!(racing_writer.write(&next)));
        manager.before_open = Some(Box::new(move || {
            if let Some(publish) = publish.take() {
                publish();
            }
        }));
        match manager.fetch_update() {
            RemoteConfigUpdate::Add { value, .. } => assert_eq!(value.path, changing),
            other => panic!("expected the current version to be added, got {other:?}"),
        }
        assert!(
            manager.failed_configs.is_empty(),
            "{:?}",
            manager.failed_configs
        );
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));
        assert_eq!(
            warnings.load(Ordering::Relaxed),
            0,
            "a stale listing is not a failure"
        );
    }

    /// Identical config files and limiter indices still refer to a new arena after restart.
    #[test]
    #[cfg(unix)]
    #[cfg_attr(miri, ignore)]
    fn a_replaced_directory_is_reapplied_even_when_identical() {
        const CHILD: &str = "DD_TEST_RC_MASTER_CHANGE";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "shm_remote_config::tests::a_replaced_directory_is_reapplied_even_when_identical"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            return;
        }
        let invariants = ConfigInvariants {
            language: "php".to_string(),
            tracer_version: "1.0.0".to_string(),
            endpoint: libdd_common::Endpoint::from_slice(&format!(
                "http://replaced-{}.invalid",
                std::process::id()
            )),
            agentless: None,
        };
        let publish = |writer: &RemoteConfigWriter, file: &NamedShmHandle| {
            let mut directory = b"runtime\n".to_vec();
            directory.extend_from_slice(unsafe { file.get_path() });
            directory.extend_from_slice(b":8:");
            directory.extend_from_slice(
                BASE64_URL_SAFE_NO_PAD
                    .encode(PATH_FIRST.to_string())
                    .as_bytes(),
            );
            directory.push(b'\n');
            assert!(writer.write(&directory));
        };
        let content = serde_json::to_vec(&dummy_dynamic_config(true)).unwrap();

        let old_file = store_shm(1, &PATH_FIRST, content.clone()).unwrap();
        let old_writer = RemoteConfigWriter::new(&invariants, &DUMMY_TARGET).unwrap();
        publish(&old_writer, &old_file);

        let mut manager = RemoteConfigManager::new(invariants.clone());
        manager.track_target(&DUMMY_TARGET);
        assert!(matches!(
            manager.fetch_update(),
            RemoteConfigUpdate::Add {
                limiter_index: 8,
                ..
            }
        ));
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));
        let old_generation = manager.current_remote_config_generation();
        assert_ne!(old_generation, 0);

        // Republish identical content under the same names, then drop the old owners.
        let new_file = store_shm(1, &PATH_FIRST, content).unwrap();
        assert_eq!(unsafe { new_file.get_path() }, unsafe {
            old_file.get_path()
        });
        let new_writer = RemoteConfigWriter::new(&invariants, &DUMMY_TARGET).unwrap();
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));
        assert_eq!(manager.current_remote_config_generation(), old_generation);
        publish(&new_writer, &new_file);
        drop(old_writer);
        drop(old_file);

        match manager.fetch_update() {
            RemoteConfigUpdate::Remove(path) => assert_eq!(&path, &*PATH_FIRST),
            other => panic!("expected the old config to be removed first, got {other:?}"),
        }
        match manager.fetch_update() {
            RemoteConfigUpdate::Add {
                value,
                limiter_index,
            } => {
                assert_eq!(value.path.config_id(), PATH_FIRST.config_id());
                assert_eq!(limiter_index, 8);
            }
            other => panic!("expected the config to be re-added, got {other:?}"),
        }
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));
        assert!(manager.current_remote_config_generation() > old_generation);

        // Unchanged content from the same writer needs no reapplication.
        publish(&new_writer, &new_file);
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));

        // Inactive targets keep their readers and follow replacements when used again.
        manager.reset_target();
        assert!(matches!(
            manager.fetch_update(),
            RemoteConfigUpdate::Remove(_)
        ));
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));
        let newest_writer = RemoteConfigWriter::new(&invariants, &DUMMY_TARGET).unwrap();
        publish(&newest_writer, &new_file);
        manager.track_target(&DUMMY_TARGET);
        assert!(matches!(
            manager.fetch_update(),
            RemoteConfigUpdate::Add {
                limiter_index: 8,
                ..
            }
        ));
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));

        let old_path = manager.get_path().unwrap().to_owned();
        crate::use_thread_sidecar_shm_namespace(Some(std::process::id()));
        manager.reconnect();
        let promoted_file = store_shm(
            1,
            &PATH_FIRST,
            serde_json::to_vec(&dummy_dynamic_config(true)).unwrap(),
        )
        .unwrap();
        let promoted_writer = RemoteConfigWriter::new(&invariants, &DUMMY_TARGET).unwrap();
        assert_ne!(manager.get_path().unwrap(), old_path.as_c_str());
        assert_eq!(
            manager.get_path().unwrap(),
            path_for_remote_config(&invariants, &DUMMY_TARGET).as_c_str()
        );
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));
        publish(&promoted_writer, &promoted_file);
        assert!(matches!(
            manager.fetch_update(),
            RemoteConfigUpdate::Remove(_)
        ));
        assert!(matches!(
            manager.fetch_update(),
            RemoteConfigUpdate::Add { .. }
        ));
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));

        manager.reset_target();
        assert!(matches!(
            manager.fetch_update(),
            RemoteConfigUpdate::Remove(_)
        ));
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));
        crate::use_thread_sidecar_shm_namespace(None);
        manager.reconnect();
        let returned_writer = RemoteConfigWriter::new(&invariants, &DUMMY_TARGET).unwrap();
        publish(&returned_writer, &new_file);
        manager.track_target(&DUMMY_TARGET);
        assert!(matches!(
            manager.fetch_update(),
            RemoteConfigUpdate::Add { .. }
        ));
        assert_eq!(manager.get_path().unwrap(), old_path.as_c_str());
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));

        manager.reset_target();
        crate::use_thread_sidecar_shm_namespace(Some(std::process::id()));
        manager.reconnect();
        assert!(matches!(
            manager.fetch_update(),
            RemoteConfigUpdate::Remove(_)
        ));
        assert!(matches!(manager.fetch_update(), RemoteConfigUpdate::None));
    }
}
