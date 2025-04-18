use std::cmp::{Ordering, PartialOrd};
use ddcommon_ffi::slice::CharSlice;
use ddcommon_ffi::Error;
use tracing::{debug, error, info, trace, warn, Event, Level as TracingLevel};
use tracing_core::Field;
use tracing_subscriber::{Registry, Layer, reload};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::filter::LevelFilter;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Once;
use std::thread;

// Define the LogLevel enum that will be exported to FFI
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum LogLevel {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
    Trace = 4,
}

// Define field key-value structure
#[repr(C)]
pub struct LogField<'a> {
    pub key: CharSlice<'a>,
    pub value: CharSlice<'a>,
}

// Update log event structure to include message
#[repr(C)]
pub struct LogEvent<'a> {
    pub level: LogLevel,
    pub message: CharSlice<'a>,
    pub fields: ddcommon_ffi::Vec<LogField<'a>>,
}

// Update callback type to match new LogEvent structure
type LogCallback = extern "C" fn(event: LogEvent);

// Update CallbackLayer to remove max_level
struct CallbackLayer {
    callback: LogCallback,
}

// Store the reload handle globally
type ReloadHandle = std::sync::Arc<reload::Handle<LevelFilter, Registry>>;
static RELOAD_HANDLE: std::sync::OnceLock<ReloadHandle> = std::sync::OnceLock::new();

impl<S> Layer<S> for CallbackLayer
where
    S: tracing::Subscriber,
{
    fn on_event<'a>(&self, event: &Event<'a>, _ctx: tracing_subscriber::layer::Context<'a, S>) {
        let level = LogLevel::from(event.metadata().level());

        #[derive(Default)]
        struct Visitor {
            message: Option<String>,
            fields: Vec<(String, String)>,
        }

        impl tracing::field::Visit for Visitor {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.message = Some(format!("{value:?}"));
                } else {
                    self.fields.push((field.name().to_string(), format!("{value:?}")));
                }
            }
        }

        let mut visitor = Visitor::default();
        event.record(&mut visitor);

        let fields = visitor.fields
            .iter()
            .map(|(key, value)| LogField {
                key: CharSlice::from(key.as_str()),
                value: CharSlice::from(value.as_str()),
            })
            .collect();

        let message = visitor.message.unwrap_or_default();
        let log_event = LogEvent {
            level,
            message: CharSlice::from(message.as_str()),
            fields: ddcommon_ffi::Vec::from_std(fields),
        };

        (self.callback)(log_event);
    }
}


// Implement From trait for level conversions
impl From<&TracingLevel> for LogLevel {
    fn from(level: &TracingLevel) -> Self {
        match level {
            &TracingLevel::TRACE => LogLevel::Trace,
            &TracingLevel::DEBUG => LogLevel::Debug,
            &TracingLevel::INFO => LogLevel::Info,
            &TracingLevel::WARN => LogLevel::Warn,
            &TracingLevel::ERROR => LogLevel::Error,
        }
    }
}

impl From<LogLevel> for LevelFilter {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Trace => LevelFilter::TRACE,
            LogLevel::Debug => LevelFilter::DEBUG,
            LogLevel::Info => LevelFilter::INFO,
            LogLevel::Warn => LevelFilter::WARN,
            LogLevel::Error => LevelFilter::ERROR,
        }
    }
}

#[no_mangle]
pub extern "C" fn ddog_logger_init(log_level: LogLevel, callback: LogCallback) -> Option<Box<Error>> {
    let (filter_layer, reload_handle) = reload::Layer::new(LevelFilter::from(log_level));
    
    // Store the reload handle globally
    let _ = RELOAD_HANDLE.set(Arc::new(reload_handle));

    let registry = Registry::default()
        .with(filter_layer)
        .with(CallbackLayer { callback });

    tracing::subscriber::set_global_default(registry)
        .map_err(|_| Box::new(Error::from("Failed to set global default subscriber")))
        .err()
}

#[no_mangle]
pub extern "C" fn ddog_logger_set_max_log_level(log_level: LogLevel) -> Option<Box<Error>> {
    RELOAD_HANDLE
        .get()
        .ok_or_else(|| Box::new(Error::from("Logger not initialized")))
        .and_then(|handle| {
            handle
                .reload(LevelFilter::from(log_level))
                .map_err(|e| Box::new(Error::from(format!("Failed to set log level: {}", e))))
        })
        .err()
}

// Add a helper function to trigger logs with all levels and template
#[no_mangle]
pub extern "C" fn trigger_logs() {
    let thread_id = std::thread::current().id();
    for i in 0..10_000 {
        info!(?thread_id, i, "Hello from background thread");
    }
}

#[no_mangle]
pub extern "C" fn trigger_logs_with_args() {
    let mut handles = Vec::new();

    for i in 0..10 {
        let handle = thread::spawn(move || {
            for j in 0..10_000 {
                info!("Hello from background thread {}: {}", i, j);
            }
        });
        handles.push(handle);
    }
    for handle in handles {
        let _ = handle.join();
    }

    for i in 0..10_000 {
        info!("Hello from main thread: {}", i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static INIT: Once = Once::new();
    static mut TEST_EVENTS: Option<Arc<Mutex<Vec<(LogLevel, Vec<(String, String)>)>>>> = None;

    fn setup_test_events() -> Arc<Mutex<Vec<(LogLevel, Vec<(String, String)>)>>> {
        let events = Arc::new(Mutex::new(Vec::new()));
        unsafe {
            TEST_EVENTS = Some(events.clone());
        }
        events
    }

    fn cleanup_test_events() {
        unsafe {
            TEST_EVENTS = None;
        }
    }

    extern "C" fn test_callback(event: LogEvent) {
        let events = unsafe { TEST_EVENTS.as_ref().unwrap() };
        let mut events = events.lock().unwrap();
        events.push((
            event.level,
            event.fields.iter()
                .map(|f| (f.key.to_string(), f.value.to_string()))
                .collect()
        ));
    }

    #[test]
    fn test_set_max_log_level_with_callback() {
        let events = setup_test_events();
        
        assert!(ddog_logger_init(LogLevel::Info, test_callback).is_none());
        assert!(ddog_logger_set_max_log_level(LogLevel::Debug).is_none());

        debug!("Debug message");
        info!("Info message");

        assert!(ddog_logger_set_max_log_level(LogLevel::Error).is_none());

        debug!("Debug message (should be filtered)");
        info!("Info message (should be filtered)");
        error!("Error message");

        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 3, "Should have captured exactly 3 messages");
        assert_eq!(captured[0].0, LogLevel::Debug);
        assert_eq!(captured[1].0, LogLevel::Info);
        assert_eq!(captured[2].0, LogLevel::Error);

        cleanup_test_events();
    }
}