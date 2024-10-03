// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::config;
use chrono::{DateTime, Utc};
use lazy_static::lazy_static;
use priority_queue::PriorityQueue;
use serde::{Deserialize, Serialize};
use std::cmp::max;
use std::collections::HashMap;
use std::hash::Hash;
use std::ops::{DerefMut, Sub};
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};
use std::{env, io};
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Id, Level, Metadata, Subscriber};
use tracing_log::LogTracer;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, MakeWriter};
use tracing_subscriber::layer::{Context, Filter, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

fn create_logfile(path: &PathBuf) -> anyhow::Result<std::fs::File> {
    let log_file = std::fs::File::options()
        .create(true)
        .truncate(false)
        .append(true)
        .open(path)?;
    Ok(log_file)
}

/// A map which refcounts a computed value given a key.
/// It will retain a reference to that value for expire_after before dropping&disabling it.
pub struct TemporarilyRetainedMap<K, V>
where
    K: TemporarilyRetainedKeyParser<V> + Clone + Eq + Hash,
{
    pub maps: RwLock<HashMap<K, V>>,
    live_counter: Mutex<HashMap<K, i32>>,
    pending_removal: Mutex<PriorityQueue<K, Instant>>,
    pub expire_after: Duration,
}

impl<K, V> Default for TemporarilyRetainedMap<K, V>
where
    K: TemporarilyRetainedKeyParser<V> + Clone + Eq + Hash,
{
    fn default() -> Self {
        TemporarilyRetainedMap {
            maps: RwLock::new(HashMap::new()),
            live_counter: Mutex::new(HashMap::new()),
            pending_removal: Mutex::new(PriorityQueue::new()),
            expire_after: Duration::from_secs(5),
        }
    }
}

unsafe impl<K, V> Sync for TemporarilyRetainedMap<K, V> where
    K: TemporarilyRetainedKeyParser<V> + Clone + Eq + Hash
{
}

#[derive(Serialize, Deserialize)]
pub struct TemporarilyRetainedMapStats {
    pub elements: u32,
    pub live_counters: u32,
    pub pending_removal: u32,
}

impl<K, V> TemporarilyRetainedMap<K, V>
where
    K: TemporarilyRetainedKeyParser<V> + Clone + Eq + Hash,
{
    pub fn add<'a, 's: 'a>(&'s self, key: K) -> TemporarilyRetainedMapGuard<'a, K, V> {
        {
            let mut live = self.live_counter.lock().unwrap();
            if let Some(count) = live.get_mut(&key) {
                *count += 1;
            } else {
                live.insert(key.clone(), 1);
                if self.pending_removal.lock().unwrap().remove(&key).is_none() {
                    self.maps.write().unwrap().insert(key.clone(), key.parse());
                    <K as TemporarilyRetainedKeyParser<V>>::enable();
                }
            }
        }

        let mut pending = self.pending_removal.lock().unwrap();
        while let Some((_, time)) = pending.peek() {
            if *time < Instant::now().sub(self.expire_after) {
                let (log_level, _) = pending.pop().unwrap();
                self.maps.write().unwrap().remove(&log_level);
                <K as TemporarilyRetainedKeyParser<V>>::disable();
            } else {
                break;
            }
        }

        TemporarilyRetainedMapGuard { key, map: self }
    }

    pub fn stats(&self) -> TemporarilyRetainedMapStats {
        TemporarilyRetainedMapStats {
            elements: self.maps.read().unwrap().len() as u32,
            live_counters: self.live_counter.lock().unwrap().len() as u32,
            pending_removal: self.pending_removal.lock().unwrap().len() as u32,
        }
    }
}

pub trait TemporarilyRetainedKeyParser<V> {
    fn parse(&self) -> V;

    fn enable() {}
    fn disable() {}
}

pub struct TemporarilyRetainedMapGuard<'a, K, V>
where
    K: TemporarilyRetainedKeyParser<V> + Clone + Eq + Hash,
{
    key: K,
    map: &'a TemporarilyRetainedMap<K, V>,
}

impl<'a, K, V> Drop for TemporarilyRetainedMapGuard<'a, K, V>
where
    K: TemporarilyRetainedKeyParser<V> + Clone + Eq + Hash,
{
    fn drop(&mut self) {
        let mut live = self.map.live_counter.lock().unwrap();
        let rc = live.get_mut(&self.key).unwrap();
        *rc -= 1;
        if *rc == 0 {
            live.remove(&self.key).unwrap();
            self.map
                .pending_removal
                .lock()
                .unwrap()
                .push(self.key.clone(), Instant::now());
        }
    }
}

/// Group EnvFilters for efficiency (to avoid checking 100s of log filters on each request and
/// recomputing call site interest all the time.
/// Ensure that the log level stays the same for at least a few seconds after session disconnect
/// in order to continue logging the sending of data submitted by the session.
pub struct MultiEnvFilter {
    map: TemporarilyRetainedMap<String, EnvFilter>,
    logs_created: Mutex<HashMap<Level, u32>>,
}

impl MultiEnvFilter {
    fn default() -> Self {
        MultiEnvFilter {
            map: TemporarilyRetainedMap::default(),
            logs_created: Mutex::new(HashMap::new()),
        }
    }

    pub fn add(&self, key: String) -> TemporarilyRetainedMapGuard<String, EnvFilter> {
        self.map.add(key)
    }

    pub fn stats(&self) -> TemporarilyRetainedMapStats {
        self.map.stats()
    }

    pub fn collect_logs_created_count(&self) -> HashMap<Level, u32> {
        let mut map = self.logs_created.lock().unwrap();
        std::mem::take(map.deref_mut())
    }
}

pub type MultiEnvFilterGuard<'a> = TemporarilyRetainedMapGuard<'a, String, EnvFilter>;

impl TemporarilyRetainedKeyParser<EnvFilter> for String {
    fn parse(&self) -> EnvFilter {
        EnvFilter::builder().parse_lossy(self)
    }

    // On change, rebuild it, according to https://docs.rs/tracing-core/0.1.32/tracing_core/callsite/fn.rebuild_interest_cache.html
    fn enable() {
        tracing::callsite::rebuild_interest_cache();
    }

    fn disable() {
        tracing::callsite::rebuild_interest_cache();
    }
}

impl<S: Subscriber> Filter<S> for &MultiEnvFilter {
    fn enabled(&self, meta: &Metadata<'_>, cx: &Context<'_, S>) -> bool {
        self.map
            .maps
            .read()
            .unwrap()
            .values()
            .any(|f| (f as &dyn Filter<S>).enabled(meta, cx))
    }

    fn callsite_enabled(&self, meta: &'static Metadata<'static>) -> Interest {
        let mut callsite_interest = Interest::never();
        for f in self.map.maps.read().unwrap().values() {
            let interest = (f as &dyn Filter<S>).callsite_enabled(meta);
            if interest.is_always() {
                return interest;
            }
            if interest.is_sometimes() {
                callsite_interest = interest;
            }
        }
        callsite_interest
    }

    fn event_enabled(&self, event: &Event<'_>, cx: &Context<'_, S>) -> bool {
        let enabled = self
            .map
            .maps
            .read()
            .unwrap()
            .values()
            .any(|f| (f as &dyn Filter<S>).event_enabled(event, cx));

        if enabled {
            let mut map = self.logs_created.lock().unwrap();
            *map.entry(event.metadata().level().to_owned()).or_default() += 1;
        }
        enabled
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        self.map
            .maps
            .read()
            .unwrap()
            .values()
            .map(|f| f.max_level_hint())
            .max()
            .flatten()
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        for f in self.map.maps.read().unwrap().values() {
            (f as &dyn Filter<S>).on_new_span(attrs, id, ctx.clone());
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        for f in self.map.maps.read().unwrap().values() {
            (f as &dyn Filter<S>).on_record(id, values, ctx.clone());
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        for f in self.map.maps.read().unwrap().values() {
            (f as &dyn Filter<S>).on_enter(id, ctx.clone());
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        for f in self.map.maps.read().unwrap().values() {
            (f as &dyn Filter<S>).on_exit(id, ctx.clone());
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        for f in self.map.maps.read().unwrap().values() {
            (f as &dyn Filter<S>).on_close(id.clone(), ctx.clone());
        }
    }
}

struct LogFormatter {
    pub base_formatter:
        tracing_subscriber::fmt::format::Format<tracing_subscriber::fmt::format::Full, ()>,
}

impl Default for LogFormatter {
    fn default() -> Self {
        LogFormatter {
            base_formatter: tracing_subscriber::fmt::format::Format::default()
                .without_time()
                .with_level(false),
        }
    }
}

// Note: specific formatter to stay in line with other ddtrace-php logs.
// May need to be adapted for other sidecar users in future.
impl<S, N> FormatEvent<S, N> for LogFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> core::fmt::Result {
        write!(
            writer,
            "[{}] [ddtrace] [{}] [sidecar] ",
            DateTime::<Utc>::from(SystemTime::now()).format("%d-%b-%Y %H:%M:%S %Z"),
            event.metadata().level().as_str().to_lowercase()
        )?;
        self.base_formatter.format_event(ctx, writer, event)
    }
}

/// Have exactly one log writer per target file.
/// Ensure that we can write for at least a few seconds after session disconnect.
pub type MultiWriter = TemporarilyRetainedMap<config::LogMethod, Box<dyn io::Write>>;
pub type MultiWriterGuard<'a> =
    TemporarilyRetainedMapGuard<'a, config::LogMethod, Box<dyn io::Write>>;

impl TemporarilyRetainedKeyParser<Box<dyn io::Write>> for config::LogMethod {
    fn parse(&self) -> Box<dyn io::Write> {
        match self {
            config::LogMethod::Stdout => Box::new(io::stdout.make_writer()),
            config::LogMethod::Stderr => Box::new(io::stderr.make_writer()),
            config::LogMethod::File(path) => create_logfile(path)
                .map_or_else::<Box<dyn io::Write>, _, _>(|_| Box::new(io::sink()), |f| Box::new(f)),
            config::LogMethod::Disabled => Box::new(io::sink()),
        }
    }
}

impl io::Write for &MultiWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[allow(clippy::manual_try_fold)] // we want the array to be fully iterated in any case
        self.maps
            .write()
            .unwrap()
            .values_mut()
            .fold(Ok(buf.len()), |cur, w| Ok(max(w.write(buf)?, cur?)))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.maps
            .write()
            .unwrap()
            .values_mut()
            .try_for_each(|w| w.flush())
    }
}

impl<'writer> MakeWriter<'writer> for &MultiWriter {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self
    }
}

lazy_static! {
    pub static ref MULTI_LOG_FILTER: MultiEnvFilter = MultiEnvFilter::default();
    pub static ref MULTI_LOG_WRITER: MultiWriter = MultiWriter::default();
}

pub(crate) fn enable_logging() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::Layer::new()
                .event_format(LogFormatter::default())
                .with_writer(&*MULTI_LOG_WRITER)
                .with_filter(&*MULTI_LOG_FILTER),
        )
        .init();

    // Set initial log level if provided
    if let Ok(env) = env::var("DD_TRACE_LOG_LEVEL") {
        MULTI_LOG_FILTER.add(env); // this also immediately drops it, but will retain it for few
                                   // seconds during startup
    }
    let config = config::Config::get();
    if !config.log_level.is_empty() {
        MULTI_LOG_FILTER.add(config.log_level.clone());
    }
    MULTI_LOG_WRITER.add(config.log_method); // same than MULTI_LOG_FILTER

    LogTracer::init()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        enable_logging, TemporarilyRetainedKeyParser, TemporarilyRetainedMap, MULTI_LOG_FILTER,
    };
    use crate::log::MultiEnvFilter;
    use lazy_static::lazy_static;
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::time::Duration;
    use tracing::subscriber::NoSubscriber;
    use tracing::{debug, error, warn, Level};
    use tracing_subscriber::layer::Filter;

    lazy_static! {
        static ref ENABLED: AtomicI32 = AtomicI32::default();
        static ref DISABLED: AtomicI32 = AtomicI32::default();
    }

    impl TemporarilyRetainedKeyParser<i32> for String {
        fn parse(&self) -> i32 {
            str::parse(self).unwrap()
        }

        fn enable() {
            ENABLED.fetch_add(1, Ordering::SeqCst);
        }

        fn disable() {
            DISABLED.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn test_refcounting_temporarily_retained_map() {
        let map = TemporarilyRetainedMap::<_, i32> {
            expire_after: Duration::from_millis(10),
            ..Default::default()
        };
        let guard1 = map.add("1".to_string());
        assert_eq!(1, *map.maps.read().unwrap().get("1").unwrap());
        assert_eq!(1, ENABLED.load(Ordering::SeqCst));

        drop(map.add("1".to_string()));

        std::thread::sleep(Duration::from_millis(10));
        let _guard2 = map.add("2".to_string());
        // still there, even after drop of one occurrence
        assert_eq!(1, *map.maps.read().unwrap().get("1").unwrap());

        drop(guard1);
        // Not immediately dropped
        assert_eq!(1, *map.maps.read().unwrap().get("1").unwrap());

        std::thread::sleep(Duration::from_millis(10));
        // still there, drop should only happen after first insertion
        assert_eq!(1, *map.maps.read().unwrap().get("1").unwrap());

        drop(map.add("2".to_string()));
        // actually dropped
        assert_eq!(None, map.maps.read().unwrap().get("1"));

        assert_eq!(1, DISABLED.load(Ordering::SeqCst));
        assert_eq!(2, ENABLED.load(Ordering::SeqCst));
    }

    #[test]
    fn test_logs_created_counter() {
        enable_logging().ok();

        MULTI_LOG_FILTER.add("warn".to_string());
        debug!("hi");
        warn!("Bim");
        warn!("Bam");
        error!("Boom");
        let map = MULTI_LOG_FILTER.collect_logs_created_count();
        assert_eq!(2, map.len());
        assert_eq!(map[&Level::WARN], 2);
        assert_eq!(map[&Level::ERROR], 1);

        debug!("hi");
        warn!("Bim");
        let map = MULTI_LOG_FILTER.collect_logs_created_count();
        assert_eq!(1, map.len());
        assert_eq!(map[&Level::WARN], 1);
    }

    #[test]
    fn test_multi_env_filter() {
        let filter = MultiEnvFilter::default();
        filter.add("warn".to_string());
        filter.add("debug".to_string());
        assert_eq!(
            Level::DEBUG,
            <&MultiEnvFilter as Filter<NoSubscriber>>::max_level_hint(&(&filter)).unwrap()
        );
    }
}
