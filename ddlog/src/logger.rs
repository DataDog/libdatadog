use std::cmp::PartialOrd;
use std::sync::Arc;
use tracing::{debug, error, info, Event, Level as TracingLevel};
use tracing_core::Field;
use tracing_subscriber::{Registry, Layer, reload};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::filter::LevelFilter;
use ddcommon_ffi::{CharSlice, Error};

/// Represents the severity levels for logging.
/// ```
/// use ddlog::logger::LogLevel;
///
/// let level = LogLevel::Info;
/// assert!(level > LogLevel::Debug);
/// ```
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    /// The "trace" level.
    ///
    /// Designates very low priority, often extremely verbose, information.
    Trace = 0,
    /// The "debug" level.
    ///
    /// Designates lower priority information.
    Debug = 1,
    /// The "info" level.
    ///
    /// Designates useful information.
    Info = 2,
    /// The "warn" level.
    ///
    /// Designates hazardous situations.
    Warn = 3,
    /// The "error" level.
    ///
    /// Designates very serious errors.
    Error = 4,
}

/// Represents a key-value pair in a log event.
#[repr(C)]
pub struct LogField<'a> {
    /// The key identifying the field
    pub key: CharSlice<'a>,
    /// The value associated with the key
    pub value: CharSlice<'a>,
}

/// Represents a complete log event with its level, message, and additional fields.
#[repr(C)]
pub struct LogEvent<'a> {
    /// The severity level of the log event
    pub level: LogLevel,
    /// The main message of the log event
    pub message: CharSlice<'a>,
    /// Additional context fields associated with the log event
    pub fields: ddcommon_ffi::Vec<LogField<'a>>,
}

/// Type alias for the C-compatible logging callback function.
pub type LogCallback = extern "C" fn(event: LogEvent);

/// A tracing layer that forwards events to a C-compatible callback.
#[repr(C)]
pub struct CallbackLayer {
    callback: LogCallback,
}

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
                // tracing uses special field named message that contains the log text
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

type ReloadHandle = Arc<reload::Handle<LevelFilter, Registry>>;
static RELOAD_HANDLE: std::sync::OnceLock<ReloadHandle> = std::sync::OnceLock::new();

/// Initializes the logger with the specified log level and callback function.
///
/// This function sets up the global logger with the given log level and callback.
/// It must be called before any logging can occur.
///
/// # Arguments
///
/// * `log_level` - The log level to capture. Events below this level will be filtered out.
/// * `callback` - The function to call for each log event that passes the level filter.
///
/// # Returns
///
/// Returns `Ok` on success, or an `Error` if the initialization fails.
pub fn logger_init(log_level: LogLevel, callback: LogCallback) -> Result<(), Error> {
    let level_filter = LevelFilter::from(log_level);
    let (filter_layer, reload_handle) = reload::Layer::new(level_filter);

    // Store the reload handle globally
    RELOAD_HANDLE
        .set(Arc::new(reload_handle))
        .map_err(|_| Error::from("Failed to set reload handle"))?;

    let registry = Registry::default()
        .with(filter_layer)
        .with(CallbackLayer { callback });

    tracing::subscriber::set_global_default(registry)
        .map_err(|_| Error::from("Failed to set global default subscriber"))
}

/// Updates the log level for the logger.
///
/// Only events at or above the specified level will be passed to the callback.
///
/// # Arguments
///
/// * `log_level` - The new log level to capture
///
/// # Returns
///
/// Returns `Ok` on success, or an `Error` if the update fails.
pub fn logger_set_log_level(log_level: LogLevel) -> Result<(), Error> {
    let level_filter = LevelFilter::from(log_level);
    RELOAD_HANDLE
        .get()
        .ok_or_else(|| Error::from("Logger not initialized"))?
        .reload(level_filter)
        .map_err(|e| Error::from(format!("Failed to set log level: {}", e)))
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, Once};
    use tracing::warn;
    use ddcommon_ffi::slice::AsBytes;
    use super::*;

    // Store owned events rather than borrowed ones
    #[derive(Debug, Clone)]
    struct StoredEvent {
        level: LogLevel,
        message: String,
        fields: Vec<(String, String)>,
    }

    static mut TEST_EVENTS: Option<Arc<Mutex<Vec<StoredEvent>>>> = None;

    fn setup_test_events() -> Arc<Mutex<Vec<StoredEvent>>> {
        let events = Arc::new(Mutex::new(Vec::new()));
        unsafe {
            TEST_EVENTS = Some(events.clone());
        }
        events
    }

    extern "C" fn test_callback(event: LogEvent) {
        let events = unsafe { TEST_EVENTS.as_ref().unwrap() };
        let mut events = events.lock().unwrap();

        // Convert to owned event before storing
        let stored = StoredEvent {
            level: event.level,
            message: event.message.try_to_utf8().unwrap().to_string(),
            fields: event.fields.as_slice().iter().map(|f| {
                (
                    f.key.try_to_utf8().unwrap().to_string(),
                    f.value.try_to_utf8().unwrap().to_string()
                )
            }).collect(),
        };
        events.push(stored);
    }

    #[test]
    fn test_set_max_log_level_with_callback() {
        let events = setup_test_events();

        // Initialize with Info level
        assert!(logger_init(LogLevel::Info, test_callback).is_ok());

        // This should not appear (below Info)
        debug!(request_id = "req1", "Debug filtered");
        // This should appear
        info!(request_id = "req2", status = 200, "Info captured");

        // Change to Error level
        assert!(logger_set_log_level(LogLevel::Error).is_ok());

        // These should not appear (below Error)
        info!(request_id = "req3", "Info filtered");
        warn!(request_id = "req4", "Warn filtered");
        // This should appear
        error!(request_id = "req5", error_code = 404, "Error captured");

        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 2, "Should have captured exactly 2 messages");

        // First message (Info)
        assert_eq!(captured[0].level, LogLevel::Info);
        assert_eq!(captured[0].message.as_str(), "Info captured");
        assert_eq!(captured[0].fields.len(), 2);
        assert!(captured[0].fields.iter().any(|(k, v)| k == "request_id" && v == "\"req2\""));
        assert!(captured[0].fields.iter().any(|(k, v)| k == "status" && v == "200"));

        // Second message (Error)
        assert_eq!(captured[1].level, LogLevel::Error);
        assert_eq!(captured[1].message.as_str(), "Error captured");
        assert_eq!(captured[1].fields.len(), 2);
        assert!(captured[1].fields.iter().any(|(k, v)| k == "request_id" && v == "\"req5\""));
        assert!(captured[1].fields.iter().any(|(k, v)| k == "error_code" && v == "404"));

        // This should error
        assert!(logger_init(LogLevel::Debug, test_callback).is_err());
    }
}