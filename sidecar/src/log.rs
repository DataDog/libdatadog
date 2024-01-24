use std::collections::HashMap;
use std::{env, io};
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
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
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

#[derive(Default)]
pub struct MultiEnvFilter {
    filters: RwLock<HashMap<String, EnvFilter>>,
    live_counter: Mutex<HashMap<String, i32>>,
    pending_removal: Mutex<PriorityQueue<String, Instant>>,
}

impl MultiEnvFilter {
    pub fn add_log_level<'a, 's: 'a>(&'s self, level: String) -> MultiEnvFilterGuard<'a> {
        {
            let mut live = self.live_counter.lock().unwrap();
            if let Some(count) = live.get_mut(level.as_str()) {
                *count += 1;
            } else {
                live.insert(level.clone(), 1);
                self.filters.write().unwrap().insert(level.clone(), EnvFilter::builder().parse_lossy(level.as_str()));
                tracing::callsite::rebuild_interest_cache();
                self.pending_removal.lock().unwrap().remove(level.as_str());
            }
        }

        let mut pending = self.pending_removal.lock().unwrap();
        while let Some((_, time)) = pending.peek() {
            if *time > Instant::now().sub(Duration::from_secs(5)) {
                let (log_level, _) = pending.pop().unwrap();
                self.filters.write().unwrap().remove(&log_level);
                tracing::callsite::rebuild_interest_cache();
            } else {
                break;
            }
        }

        MultiEnvFilterGuard { level, filter: self }
    }
}

pub struct MultiEnvFilterGuard<'a> {
    level: String,
    filter: &'a MultiEnvFilter
}

impl<'a> Drop for MultiEnvFilterGuard<'a> {
    fn drop(&mut self) {
        let mut live = self.filter.live_counter.lock().unwrap();
        let rc = live.get_mut(self.level.as_str()).unwrap();
        *rc -= 1;
        if *rc == 0 {
            live.remove(self.level.as_str()).unwrap();
            self.filter.pending_removal.lock().unwrap().push(self.level.clone(), Instant::now());
        }
    }
}

impl<S: Subscriber> Filter<S> for &MultiEnvFilter {
    fn enabled(&self, meta: &Metadata<'_>, cx: &Context<'_, S>) -> bool {
        self.filters.read().unwrap().values().any(|f| (f as &dyn Filter<S>).enabled(meta, cx))
    }

    fn callsite_enabled(&self, meta: &'static Metadata<'static>) -> Interest {
        let mut callsite_interest = Interest::never();
        for f in self.filters.read().unwrap().values() {
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
        self.filters.read().unwrap().values().any(|f| (f as &dyn Filter<S>).event_enabled(event, cx))
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        self.filters.read().unwrap().values().map(|f| f.max_level_hint()).min().flatten()
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        for f in self.filters.read().unwrap().values() {
            (f as &dyn Filter<S>).on_new_span(attrs, id, ctx.clone());
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        for f in self.filters.read().unwrap().values() {
            (f as &dyn Filter<S>).on_record(id,values, ctx.clone());
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        for f in self.filters.read().unwrap().values() {
            (f as &dyn Filter<S>).on_enter(id, ctx.clone());
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        for f in self.filters.read().unwrap().values() {
            (f as &dyn Filter<S>).on_exit(id, ctx.clone());
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        for f in self.filters.read().unwrap().values() {
            (f as &dyn Filter<S>).on_close(id.clone(), ctx.clone());
        }
    }
}

lazy_static! {
    pub static ref MULTI_LOG_FILTER: MultiEnvFilter = MultiEnvFilter::default();
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

pub(crate) fn enable_logging() -> anyhow::Result<()> {
    let registry = tracing_subscriber::registry();

    let subscriber = tracing_subscriber::fmt::Layer::new()
        .event_format(LogFormatter::default());
    match config::Config::get().log_method {
        config::LogMethod::Stdout => registry.with(subscriber.with_writer(io::stdout).with_filter(&*MULTI_LOG_FILTER)).init(),
        config::LogMethod::Stderr => registry.with(subscriber.with_writer(io::stderr).with_filter(&*MULTI_LOG_FILTER)).init(),
        config::LogMethod::File(path) => registry.with(subscriber.with_writer(Mutex::new(create_logfile(&path)?)).with_filter(&*MULTI_LOG_FILTER)).init(),
        config::LogMethod::Disabled => return Ok(()),
    };

    // Set initial log level if provided
    if let Ok(env) = env::var("DD_TRACE_LOG_LEVEL") {
        MULTI_LOG_FILTER.add_log_level(env);
    }

    LogTracer::init()?;

    Ok(())
}
