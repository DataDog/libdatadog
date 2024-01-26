use std::collections::HashMap;
use std::{env, io};
use std::cmp::max;
use std::hash::Hash;
use std::ops::Sub;
use std::path::PathBuf;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};
use chrono::{DateTime, Utc};
use lazy_static::lazy_static;
use priority_queue::PriorityQueue;
use tracing::{Event, Id, Metadata, Subscriber};
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Record};
use tracing::subscriber::Interest;
use tracing_log::LogTracer;
use tracing_subscriber::{EnvFilter, Layer};
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, MakeWriter};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::layer::{Context, Filter, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use crate::config;

fn create_logfile(path: &PathBuf) -> anyhow::Result<std::fs::File> {
    let log_file = std::fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .append(true)
        .open(path)?;
    Ok(log_file)
}

pub struct TemporarilyRetainedMap<K, V> where K: TemporarilyRetainedKeyParser<V> + Clone + Eq + Hash {
    maps: RwLock<HashMap<K, V>>,
    live_counter: Mutex<HashMap<K, i32>>,
    pending_removal: Mutex<PriorityQueue<K, Instant>>,
}

impl<K, V> Default for TemporarilyRetainedMap<K, V> where K: TemporarilyRetainedKeyParser<V> + Clone + Eq + Hash {
    fn default() -> Self {
        TemporarilyRetainedMap {
            maps: RwLock::new(HashMap::new()),
            live_counter: Mutex::new(HashMap::new()),
            pending_removal: Mutex::new(PriorityQueue::new()),
        }
    }
}

unsafe impl<K, V> Sync for TemporarilyRetainedMap<K, V> where K: TemporarilyRetainedKeyParser<V> + Clone + Eq + Hash {}

impl<K, V> TemporarilyRetainedMap<K, V> where K: TemporarilyRetainedKeyParser<V> + Clone + Eq + Hash {
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
            if *time > Instant::now().sub(Duration::from_secs(5)) {
                let (log_level, _) = pending.pop().unwrap();
                self.maps.write().unwrap().remove(&log_level);
                <K as TemporarilyRetainedKeyParser<V>>::disable();
            } else {
                break;
            }
        }

        TemporarilyRetainedMapGuard { key, map: self }
    }
}

pub trait TemporarilyRetainedKeyParser<V> {
    fn parse(&self) -> V;

    fn enable() {}
    fn disable() {}
}

pub struct TemporarilyRetainedMapGuard<'a, K, V> where K: TemporarilyRetainedKeyParser<V> + Clone + Eq + Hash {
    key: K,
    map: &'a TemporarilyRetainedMap<K, V>
}

impl<'a, K, V> Drop for TemporarilyRetainedMapGuard<'a, K, V> where K: TemporarilyRetainedKeyParser<V> + Clone + Eq + Hash {
    fn drop(&mut self) {
        let mut live = self.map.live_counter.lock().unwrap();
        let rc = live.get_mut(&self.key).unwrap();
        *rc -= 1;
        if *rc == 0 {
            live.remove(&self.key).unwrap();
            self.map.pending_removal.lock().unwrap().push(self.key.clone(), Instant::now());
        }
    }
}

pub type MultiEnvFilter = TemporarilyRetainedMap<String, EnvFilter>;
pub type MultiEnvFilterGuard<'a> = TemporarilyRetainedMapGuard<'a, String, EnvFilter>;

impl TemporarilyRetainedKeyParser<EnvFilter> for String {
    fn parse(&self) -> EnvFilter {
        EnvFilter::builder().parse_lossy(self)
    }

    fn enable() {
        tracing::callsite::rebuild_interest_cache();
    }

    fn disable() {
        tracing::callsite::rebuild_interest_cache();
    }
}

impl<S: Subscriber> Filter<S> for &MultiEnvFilter {
    fn enabled(&self, meta: &Metadata<'_>, cx: &Context<'_, S>) -> bool {
        self.maps.read().unwrap().values().any(|f| (f as &dyn Filter<S>).enabled(meta, cx))
    }

    fn callsite_enabled(&self, meta: &'static Metadata<'static>) -> Interest {
        let mut callsite_interest = Interest::never();
        for f in self.maps.read().unwrap().values() {
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
        self.maps.read().unwrap().values().any(|f| (f as &dyn Filter<S>).event_enabled(event, cx))
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        self.maps.read().unwrap().values().map(|f| f.max_level_hint()).min().flatten()
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        for f in self.maps.read().unwrap().values() {
            (f as &dyn Filter<S>).on_new_span(attrs, id, ctx.clone());
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        for f in self.maps.read().unwrap().values() {
            (f as &dyn Filter<S>).on_record(id,values, ctx.clone());
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        for f in self.maps.read().unwrap().values() {
            (f as &dyn Filter<S>).on_enter(id, ctx.clone());
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        for f in self.maps.read().unwrap().values() {
            (f as &dyn Filter<S>).on_exit(id, ctx.clone());
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        for f in self.maps.read().unwrap().values() {
            (f as &dyn Filter<S>).on_close(id.clone(), ctx.clone());
        }
    }
}

struct LogFormatter {
    pub base_formatter: tracing_subscriber::fmt::format::Format<tracing_subscriber::fmt::format::Full, ()>,
}

impl Default for LogFormatter {
    fn default() -> Self {
        LogFormatter {
            base_formatter: tracing_subscriber::fmt::format::Format::default().without_time().with_level(false)
        }
    }
}

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
        write!(writer, "[{}] [ddtrace] [{}] [sidecar] ",
               DateTime::<Utc>::from(SystemTime::now()).format("%d-%b-%Y %H:%M:%S %Z"),
               event.metadata().level().as_str().to_lowercase())?;
        self.base_formatter.format_event(ctx, writer, event)
    }
}

pub type MultiWriter = TemporarilyRetainedMap<config::LogMethod, Box<dyn io::Write>>;
pub type MultiWriterGuard<'a> = TemporarilyRetainedMapGuard<'a, config::LogMethod, Box<dyn io::Write>>;

impl TemporarilyRetainedKeyParser<Box<dyn io::Write>> for config::LogMethod {
    fn parse(&self) -> Box<dyn io::Write> {
        match self {
            config::LogMethod::Stdout => Box::new(io::stdout.make_writer()),
            config::LogMethod::Stderr => Box::new(io::stderr.make_writer()),
            config::LogMethod::File(path) => create_logfile(&path).map_or_else::<Box<dyn io::Write>, _, _>(|_| Box::new(io::sink()), |f| Box::new(f)),
            config::LogMethod::Disabled => Box::new(io::sink()),
        }
    }
}

impl io::Write for &MultiWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.maps.write().unwrap().values_mut().fold(Ok(buf.len()), |cur, w| Ok(max(w.write(buf)?, cur?)))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.maps.write().unwrap().values_mut().try_for_each(|w| w.flush())
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
        .with(tracing_subscriber::fmt::Layer::new()
        .event_format(LogFormatter::default())
        .with_writer(&*MULTI_LOG_WRITER)
        .with_filter(&*MULTI_LOG_FILTER)).init();

    // Set initial log level if provided
    if let Ok(env) = env::var("DD_TRACE_LOG_LEVEL") {
        MULTI_LOG_FILTER.add(env); // this also immediately drops it, but will retain it for few seconds during startup
    }
    MULTI_LOG_WRITER.add(config::Config::get().log_method); // same than MULTI_LOG_FILTER

    LogTracer::init()?;

    Ok(())
}
