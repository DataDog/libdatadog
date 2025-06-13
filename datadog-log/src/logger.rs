// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::writers::{FileWriter, StdWriter};
use ddcommon_ffi::Error;
use std::sync::{LazyLock, Mutex};
use tracing::subscriber::DefaultGuard;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Layered, SubscriberExt};
use tracing_subscriber::reload::Handle;
use tracing_subscriber::{fmt, reload, EnvFilter, Layer, Registry};

/// Log level for filtering log events.
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogEventLevel {
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

/// Configuration for file-based logging.
pub struct FileConfig {
    /// Path where log files will be written.
    pub path: String,
    /// Maximum size in bytes for each log file.
    /// Set to 0 to disable size-based rotation.
    pub max_size_bytes: u64,
    /// Maximum total number of files (current + rotated) to keep on disk.
    /// When this limit is exceeded, the oldest rotated files are deleted.
    /// Set to 0 to disable file cleanup.
    pub max_files: u64,
}

/// Target for standard stream output.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub enum StdTarget {
    /// Write to standard output (stdout).
    Out,
    /// Write to standard error (stderr).
    Err,
}

/// Configuration for standard stream logging.
pub struct StdConfig {
    /// Target stream (stdout or stderr).
    pub target: StdTarget,
}

/// Logger with layer-based architecture.
struct Logger {
    /// Handle for modifying the log layers at runtime.
    /// Complex type definition causes issues with cbindgen, so we suppress clippy's type
    /// complexity warning.
    #[allow(clippy::type_complexity)]
    layer_handle: Handle<
        Vec<Box<dyn Layer<Layered<reload::Layer<EnvFilter, Registry>, Registry>> + Send + Sync>>,
        Layered<reload::Layer<EnvFilter, Registry>, Registry>,
    >,
    /// Handle for modifying the log filter at runtime.
    filter_handle: Handle<EnvFilter, Registry>,
    /// Guard is for local subscriber which is not used in the global logger.
    #[allow(dead_code)]
    _guard: Option<DefaultGuard>,
    /// File configuration.
    file_config: Option<FileConfig>,
    /// Standard stream configuration.
    std_config: Option<StdConfig>,
}

impl Logger {
    #[cfg(test)]
    fn setup() -> Result<Self, Error> {
        Self::setup_with_global(false)
    }

    fn setup_global() -> Result<Self, Error> {
        Self::setup_with_global(true)
    }

    fn setup_with_global(global: bool) -> Result<Self, Error> {
        let layers = vec![];
        let env_filter = env_filter();
        let (filter_layer, filter_handle) = reload::Layer::new(env_filter);
        let (layers_layer, layer_handle) = reload::Layer::new(layers);

        let subscriber = tracing_subscriber::registry()
            .with(filter_layer)
            .with(layers_layer);

        if global {
            match tracing::subscriber::set_global_default(subscriber) {
                Ok(_) => Ok(Self {
                    layer_handle,
                    filter_handle,
                    _guard: None,
                    file_config: None,
                    std_config: None,
                }),
                Err(_e) => Err(Error::from("Failed to set global default subscriber")),
            }
        } else {
            Ok(Self {
                layer_handle,
                filter_handle,
                _guard: Some(tracing::subscriber::set_default(subscriber)),
                file_config: None,
                std_config: None,
            })
        }
    }

    fn configure(&self) -> Result<(), Error> {
        self.layer_handle
            .modify(|layers| {
                // Clear existing layers first
                // since we can't selectively replace them because of the dynamic nature of the
                // layers. This is necessary to avoid accumulating layers on each
                // configuration call.
                layers.clear();

                // Add file layer if configured
                if let Some(file_config) = &self.file_config {
                    if let Ok(file_layer) = file_layer(file_config) {
                        layers.push(file_layer);
                    }
                }

                if let Some(std_config) = &self.std_config {
                    if let Ok(std_layer) = std_layer(std_config) {
                        layers.push(std_layer);
                    }
                }
            })
            .map_err(|e| Error::from(format!("Failed to update logger configuration: {}", e)))?;

        Ok(())
    }

    fn disable_file(&mut self) -> Result<(), Error> {
        self.file_config = None;
        self.configure()
    }

    fn configure_file(&mut self, file_config: FileConfig) -> Result<(), Error> {
        self.file_config = Some(file_config);
        self.configure()
    }

    fn disable_std(&mut self) -> Result<(), Error> {
        self.std_config = None;
        self.configure()
    }

    fn configure_std(&mut self, std_config: StdConfig) -> Result<(), Error> {
        self.std_config = Some(std_config);
        self.configure()
    }

    /// Set the log level for the logger.
    fn set_log_level(&self, log_level: LogEventLevel) -> Result<(), Error> {
        let level_filter = LevelFilter::from(log_level);
        let new_filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(level_filter.to_string().to_lowercase()));

        self.filter_handle
            .modify(|filter| {
                *filter = new_filter;
            })
            .map_err(|e| Error::from(format!("Failed to update log level: {}", e)))?;

        Ok(())
    }
}

/// Create environment filter with default to INFO level.
fn env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(LevelFilter::INFO.to_string().to_lowercase()))
}

/// Create standard output layer.
#[allow(clippy::type_complexity)]
fn std_layer(
    config: &StdConfig,
) -> Result<
    Box<dyn Layer<Layered<reload::Layer<EnvFilter, Registry>, Registry>> + Send + Sync + 'static>,
    Error,
> {
    let writer = StdWriter::new(config.target);

    Ok(fmt::layer()
        .with_writer(writer)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_ansi(false)
        .boxed())
}

#[allow(clippy::type_complexity)]
fn file_layer(
    config: &FileConfig,
) -> Result<
    Box<dyn Layer<Layered<reload::Layer<EnvFilter, Registry>, Registry>> + Send + Sync + 'static>,
    Error,
> {
    let writer = FileWriter::new(config)
        .map_err(|e| Error::from(format!("Failed to create file writer: {}", e)))?;

    Ok(fmt::layer()
        .with_writer(writer)
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_ansi(false)
        .json()
        .boxed())
}

impl From<LogEventLevel> for LevelFilter {
    fn from(level: LogEventLevel) -> Self {
        match level {
            LogEventLevel::Trace => LevelFilter::TRACE,
            LogEventLevel::Debug => LevelFilter::DEBUG,
            LogEventLevel::Info => LevelFilter::INFO,
            LogEventLevel::Warn => LevelFilter::WARN,
            LogEventLevel::Error => LevelFilter::ERROR,
        }
    }
}

static LOGGER: LazyLock<Mutex<Option<Logger>>> = LazyLock::new(|| Mutex::new(None));

/// Configures the global logger to write to a file in JSON format.
///
/// # Arguments
/// * `file_config` - Configuration specifying the file path
pub fn logger_configure_file(file_config: FileConfig) -> Result<(), Error> {
    let logger_mutex = &LOGGER;
    let mut logger_guard = logger_mutex
        .lock()
        .map_err(|e| Error::from(format!("Failed to acquire logger lock: {}", e)))?;

    if let Some(logger) = logger_guard.as_mut() {
        logger.configure_file(file_config)
    } else {
        let mut logger = Logger::setup_global()?;
        logger.configure_file(file_config)?;
        *logger_guard = Some(logger);
        Ok(())
    }
}

/// Disables file logging for the global logger.
///
/// Removes file logging configuration while keeping other outputs (like std streams) active.
pub fn logger_disable_file() -> Result<(), Error> {
    let logger_mutex = &LOGGER;
    let mut logger_guard = logger_mutex
        .lock()
        .map_err(|e| Error::from(format!("Failed to acquire logger lock: {}", e)))?;

    if let Some(logger) = logger_guard.as_mut() {
        logger.disable_file()
    } else {
        Err(Error::from("Logger not initialized"))
    }
}

/// Configures the global logger to write to stdout or stderr in compact format.
///
/// # Arguments
/// * `std_config` - Configuration specifying stdout or stderr
pub fn logger_configure_std(std_config: StdConfig) -> Result<(), Error> {
    let logger_mutex = &LOGGER;
    let mut logger_guard = logger_mutex
        .lock()
        .map_err(|e| Error::from(format!("Failed to acquire logger lock: {}", e)))?;

    if let Some(logger) = logger_guard.as_mut() {
        logger.configure_std(std_config)
    } else {
        let mut logger = Logger::setup_global()?;
        logger.configure_std(std_config)?;
        *logger_guard = Some(logger);
        Ok(())
    }
}

/// Disables standard stream logging for the global logger.
///
/// Removes std stream logging configuration while keeping other outputs (like file) active.
pub fn logger_disable_std() -> Result<(), Error> {
    let logger_mutex = &LOGGER;
    let mut logger_guard = logger_mutex
        .lock()
        .map_err(|e| Error::from(format!("Failed to acquire logger lock: {}", e)))?;

    if let Some(logger) = logger_guard.as_mut() {
        logger.disable_std()
    } else {
        Err(Error::from("Logger not initialized"))
    }
}

/// Sets the minimum log level for the global logger.
///
/// # Arguments
/// * `log_level` - Minimum level (Trace, Debug, Info, Warn, Error)
pub fn logger_set_log_level(log_level: LogEventLevel) -> Result<(), Error> {
    let logger_mutex = &LOGGER;
    let logger_guard = logger_mutex
        .lock()
        .map_err(|e| Error::from(format!("Failed to acquire logger lock: {}", e)))?;

    if let Some(logger) = logger_guard.as_ref() {
        logger.set_log_level(log_level)
    } else {
        Err(Error::from("Logger not initialized"))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use tracing::field::{Field, Visit};
    use tracing::subscriber::Interest;
    use tracing::{debug, error, info, trace, warn, Event, Metadata, Subscriber};
    use tracing_subscriber::layer::{Context, Layer};

    use super::*;

    #[derive(Default)]
    struct MessageVisitor {
        message: Option<String>,
        all_fields: std::collections::HashMap<String, String>,
    }

    impl Visit for MessageVisitor {
        fn record_i64(&mut self, field: &Field, value: i64) {
            let field_name = field.name();
            let field_value = value.to_string();
            self.all_fields
                .insert(field_name.to_string(), field_value.clone());

            if field_name == "message" {
                self.message = Some(field_value);
            }
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            let field_name = field.name();
            let field_value = value.to_string();
            self.all_fields
                .insert(field_name.to_string(), field_value.clone());

            if field_name == "message" {
                self.message = Some(field_value);
            }
        }

        fn record_bool(&mut self, field: &Field, value: bool) {
            let field_name = field.name();
            let field_value = value.to_string();
            self.all_fields
                .insert(field_name.to_string(), field_value.clone());

            if field_name == "message" {
                self.message = Some(field_value);
            }
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            let field_name = field.name();
            self.all_fields
                .insert(field_name.to_string(), value.to_string());

            if field_name == "message" {
                self.message = Some(value.to_string());
            }
        }

        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let field_name = field.name();
            let field_value = format!("{:?}", value);
            self.all_fields
                .insert(field_name.to_string(), field_value.clone());

            if field_name == "message" {
                self.message = Some(field_value);
            }
        }
    }

    #[derive(Default)]
    struct RecordingLayer<S> {
        events: Arc<Mutex<Vec<String>>>,
        _subscriber: std::marker::PhantomData<S>,
    }

    impl<S> RecordingLayer<S> {
        fn new(events: Arc<Mutex<Vec<String>>>) -> Self {
            RecordingLayer {
                events,
                _subscriber: std::marker::PhantomData,
            }
        }
    }

    impl<S> Layer<S> for RecordingLayer<S>
    where
        S: Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
            Interest::always()
        }

        fn enabled(&self, _metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
            true
        }

        fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
            let mut visitor = MessageVisitor::default();
            event.record(&mut visitor);

            let mut events = self.events.lock().unwrap();
            let message = visitor.message.unwrap_or_else(|| {
                // If no explicit message field, try to reconstruct from all fields
                if !visitor.all_fields.is_empty() {
                    format!("Fields: {:?}", visitor.all_fields)
                } else {
                    format!(
                        "Event: {} - {}",
                        event.metadata().target(),
                        event.metadata().name()
                    )
                }
            });
            events.push(message);
        }
    }
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_logger_setup() {
        let logger = Logger::setup();
        assert!(logger.is_ok(), "Logger setup should succeed");
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_logger_with_std() {
        let events: Arc<Mutex<Vec<String>>> = Default::default();
        let mut logger = Logger::setup().expect("Should setup logger successfully");

        let std_config = StdConfig {
            target: StdTarget::Out,
        };

        logger
            .configure_std(std_config)
            .expect("Should configure std output");

        // Add recording layer after configuration
        logger
            .layer_handle
            .modify(|layers| {
                layers.push(Box::new(RecordingLayer::new(Arc::clone(&events))));
            })
            .expect("Should be able to add recording layer");

        logger
            .set_log_level(LogEventLevel::Info)
            .expect("Should set log level to Info");

        info!(message = "Std output test message");

        let captured_events = events.lock().unwrap();
        assert_eq!(
            captured_events.len(),
            1,
            "Should capture message with std output"
        );
        assert_eq!(captured_events[0], "Std output test message");

        drop(logger);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_logger_with_file() {
        let events: Arc<Mutex<Vec<String>>> = Default::default();
        let mut logger = Logger::setup().expect("Should setup logger successfully");

        let temp_dir = TempDir::new().expect("Should create temp directory");
        let log_path = temp_dir.path().join("test.log");

        let file_config = FileConfig {
            path: log_path.to_string_lossy().to_string(),
            max_files: 0,
            max_size_bytes: 0,
        };

        logger
            .configure_file(file_config)
            .expect("Should configure file output");

        // Add recording layer after configuration
        logger
            .layer_handle
            .modify(|layers| {
                layers.push(Box::new(RecordingLayer::new(Arc::clone(&events))));
            })
            .expect("Should be able to add recording layer");

        logger
            .set_log_level(LogEventLevel::Info)
            .expect("Should set log level to Info");

        info!(message = "File output test message");

        let captured_events = events.lock().unwrap();
        assert_eq!(
            captured_events.len(),
            1,
            "Should capture message with file output"
        );
        assert_eq!(captured_events[0], "File output test message");
        drop(captured_events);

        assert!(
            log_path.exists(),
            "Log file should be created at {:?}",
            log_path
        );

        // add delay to ensure file is written
        std::thread::sleep(std::time::Duration::from_millis(100));

        if let Ok(content) = std::fs::read_to_string(&log_path) {
            assert!(
                !content.is_empty(),
                "Log file should contain some log output"
            );
        }

        drop(logger);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_logger_with_std_and_file() {
        let events: Arc<Mutex<Vec<String>>> = Default::default();
        let mut logger = Logger::setup().expect("Should setup logger successfully");

        // Configure std output
        let std_config = StdConfig {
            target: StdTarget::Err,
        };
        logger
            .configure_std(std_config)
            .expect("Should configure std output");

        let temp_dir = TempDir::new().expect("Should create temp directory");
        let log_path = temp_dir.path().join("test.log");
        let file_config = FileConfig {
            path: log_path.to_string_lossy().to_string(),
            max_size_bytes: 0,
            max_files: 0,
        };
        logger
            .configure_file(file_config)
            .expect("Should configure file output");

        // Add recording layer after configuration
        logger
            .layer_handle
            .modify(|layers| {
                layers.push(Box::new(RecordingLayer::new(Arc::clone(&events))));
            })
            .expect("Should be able to add recording layer");

        logger
            .set_log_level(LogEventLevel::Info)
            .expect("Should set log level to Info");

        warn!(message = "Std and file output test message");

        let captured_events = events.lock().unwrap();
        assert_eq!(
            captured_events.len(),
            1,
            "Should capture message with std and file output"
        );
        assert_eq!(captured_events[0], "Std and file output test message");
        drop(captured_events);

        // Verify that the log file was created
        assert!(
            log_path.exists(),
            "Log file should be created at {:?}",
            log_path
        );

        drop(logger);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_logger_level_change() {
        let events: Arc<Mutex<Vec<String>>> = Default::default();
        let logger = Logger::setup().expect("Should setup logger successfully");

        // Add recording layer
        logger
            .layer_handle
            .modify(|layers| {
                layers.push(Box::new(RecordingLayer::new(Arc::clone(&events))));
            })
            .expect("Should be able to add recording layer");

        // Test TRACE level (captures everything)
        logger
            .set_log_level(LogEventLevel::Trace)
            .expect("Should set log level to Trace");

        trace!(message = "Trace message");
        debug!(message = "Debug message");
        info!(message = "Info message");
        warn!(message = "Warn message");
        error!(message = "Error message");

        {
            let captured_events = events.lock().unwrap();
            assert_eq!(
                captured_events.len(),
                5,
                "Should capture all 5 messages at TRACE level"
            );
        }

        // Clear and test WARN level (only WARN and ERROR)
        events.lock().unwrap().clear();
        logger
            .set_log_level(LogEventLevel::Warn)
            .expect("Should set log level to Warn");

        trace!(message = "Trace filtered");
        debug!(message = "Debug filtered");
        info!(message = "Info filtered");
        warn!(message = "Warn message");
        error!(message = "Error message");

        {
            let captured_events = events.lock().unwrap();
            assert_eq!(
                captured_events.len(),
                2,
                "Should capture only WARN and ERROR messages"
            );
            assert_eq!(captured_events[0], "Warn message");
            assert_eq!(captured_events[1], "Error message");
        }

        // Clear and test ERROR level (only ERROR)
        events.lock().unwrap().clear();
        logger
            .set_log_level(LogEventLevel::Error)
            .expect("Should set log level to Error");

        trace!(message = "Trace filtered");
        debug!(message = "Debug filtered");
        info!(message = "Info filtered");
        warn!(message = "Warn filtered");
        error!(message = "Error message");

        {
            let captured_events = events.lock().unwrap();
            assert_eq!(
                captured_events.len(),
                1,
                "Should capture only ERROR message"
            );
            assert_eq!(captured_events[0], "Error message");
        }

        drop(logger);
    }
}
