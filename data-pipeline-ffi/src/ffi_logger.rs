use std::cmp::{Ordering, PartialOrd};
use ddcommon_ffi::slice::{AsBytes, CharSlice};
use ddcommon_ffi::Error;
use std::sync::Mutex;
use tracing::{debug, error, info, trace, warn, Event, Level as TracingLevel};
use tracing_core::Field;
use tracing_subscriber::{Registry, Layer};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::filter::LevelFilter;

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

// Implement PartialOrd and Ord for LogLevel
impl PartialOrd for LogLevel {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        // Define the ordering relationship between log levels
        // We need to manually define this because the numeric values
        // don't match the severity ordering (Trace should be less severe than Debug)
        match (*self, *other) {
            (LogLevel::Trace, LogLevel::Trace) => Some(Ordering::Equal),
            (LogLevel::Trace, _) => Some(Ordering::Less),
            (LogLevel::Debug, LogLevel::Trace) => Some(Ordering::Greater),
            (LogLevel::Debug, LogLevel::Debug) => Some(Ordering::Equal),
            (LogLevel::Debug, _) => Some(Ordering::Less),
            (LogLevel::Info, LogLevel::Trace | LogLevel::Debug) => Some(Ordering::Greater),
            (LogLevel::Info, LogLevel::Info) => Some(Ordering::Equal),
            (LogLevel::Info, _) => Some(Ordering::Less),
            (LogLevel::Warn, LogLevel::Error) => Some(Ordering::Less),
            (LogLevel::Warn, _) => Some(Ordering::Greater),
            (LogLevel::Error, LogLevel::Error) => Some(Ordering::Equal),
            (LogLevel::Error, _) => Some(Ordering::Greater),
        }
    }
}

impl Ord for LogLevel {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap()
    }
}

// Define a log message structure to pass to callbacks
#[repr(C)]
pub struct LogMessage<'a> {
    pub template: CharSlice<'a>,
    pub args: ddcommon_ffi::Vec<CharSlice<'a>>,
    pub level: LogLevel,
}

// Update callback type to use FFIVec<CharSlice>
type LogCallback = extern "C" fn(level: LogLevel, template: CharSlice, args: ddcommon_ffi::Vec<CharSlice>);

// Define the CallbackLayer struct
struct CallbackLayer {
    callback: Option<LogCallback>,
}

// Update the implementation of CallbackLayer
impl<S> Layer<S> for CallbackLayer
where
    S: tracing::Subscriber,
{
    fn on_event<'a>(&self, event: &Event<'a>, _ctx: tracing_subscriber::layer::Context<'a, S>) {
        // Log level
        let level = convert_tracing_level_to_log_level(event.metadata().level());
        println!("EVENT LEVEL: {:?}", level);
        
        // Use a visitor to collect the fields and extract args
        #[derive(Default)]
        struct Visitor {
            message: String,
            args: Vec<String>,
            fields: Vec<String>,
        }
        
        impl tracing::field::Visit for Visitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.message = format!("{value:?}");
                } else if field.name().starts_with("arg") {
                    // Capture arguments separately
                    self.args.push(format!("{value:?}"));
                } else {
                    self.fields.push(format!("{field}={value:?}"));
                }
            }

            fn record_str(&mut self, field: &Field, value: &str) {
                if field.name() == "message" {
                    self.message = value.to_string();
                } else if field.name().starts_with("arg") {
                    // Capture arguments separately
                    self.args.push(value.to_string());
                } else {
                    self.fields.push(format!("{field}={value}"));
                }
            }
        }

        let mut visitor = Visitor::default();
        event.record(&mut visitor);
        
        // Try to extract the original template format from the message
        // This is an approximation - not 100% accurate for all cases
        let template_format = if visitor.message.contains(": ") && !visitor.args.is_empty() {
            // For messages like "This is a log with args: arg1"
            let parts: Vec<&str> = visitor.message.split(": ").collect();
            if parts.len() > 1 {
                format!("{}: {{}}", parts[0])
            } else {
                visitor.message.clone()
            }
        } else {
            visitor.message.clone()
        };
        
        // Build the template with target
        let display_template = format!("[{}] {}", event.metadata().target(), template_format);
        
        // Print the template and args separately for debugging
        println!("DEBUG - ORIGINAL TEMPLATE: {}", display_template);
        
        if !visitor.args.is_empty() {
            println!("DEBUG - ARGS: [{}]", visitor.args.join(", "));
        } else {
            println!("DEBUG - ARGS: <none>");
        }
        
        if !visitor.fields.is_empty() {
            println!("DEBUG - FIELDS: [{}]", visitor.fields.join(", "));
        } else {
            println!("DEBUG - FIELDS: <none>");
        }
        
        // If there's a callback, we'd call it here, but for now just print
        if let Some(_callback) = self.callback {
            println!("DEBUG - WOULD CALL CALLBACK WITH ABOVE VALUES");
        }
        
        // Also print the full message as before
        let mut full_message = format!("[{}] {}", event.metadata().target(), visitor.message);
        if !visitor.fields.is_empty() {
            full_message.push_str(", ");
            full_message.push_str(&visitor.fields.join(", "));
        }
        println!("FULL MESSAGE: {}", full_message);
        
        // Add a separator for clarity
        println!("----------------------------------------");
    }
}

// Helper function to convert tracing level to our log level
fn convert_tracing_level_to_log_level(level: &TracingLevel) -> LogLevel {
    match *level {
        TracingLevel::TRACE => LogLevel::Trace,
        TracingLevel::DEBUG => LogLevel::Debug,
        TracingLevel::INFO => LogLevel::Info,
        TracingLevel::WARN => LogLevel::Warn,
        TracingLevel::ERROR => LogLevel::Error,
    }
}

// Convert our LogLevel to tracing LevelFilter
fn convert_log_level_to_level_filter(level: LogLevel) -> LevelFilter {
    match level {
        LogLevel::Trace => LevelFilter::TRACE,
        LogLevel::Debug => LevelFilter::DEBUG,
        LogLevel::Info => LevelFilter::INFO,
        LogLevel::Warn => LevelFilter::WARN,
        LogLevel::Error => LevelFilter::ERROR,
    }
}

#[no_mangle]
pub extern "C" fn ddog_logger_init(log_level: LogLevel, callback: Option<LogCallback>) -> Option<Box<Error>> {
    let level_filter = convert_log_level_to_level_filter(log_level);

    let callback_layer = CallbackLayer {
        callback,
    };

    // Create the registry with our callback layer
    let registry = Registry::default()
        .with(callback_layer);

    match tracing::subscriber::set_global_default(registry) {
        Ok(_) => None,
        Err(_) => {
            // Handle error by converting to Error type
            Some(Box::new(Error::from("Failed to set global default subscriber")))
        }
    }
}

// Add a method to set max log level
#[no_mangle]
pub extern "C" fn ddog_ffi_logger_set_max_log_level(log_level: LogLevel) {
    // This is a simplification - in a real implementation, you'd need to
    // interact with the global subscriber to change its max level
    // This might require restructuring how the subscriber is set up
}

#[no_mangle]
pub extern "C" fn ddog_logger_set_log_level(log_level: LogLevel) -> Option<Box<Error>> {
    let level_filter = convert_log_level_to_level_filter(log_level);

    let subscriber = tracing_subscriber::fmt::Subscriber::builder()
        .with_max_level(level_filter)
        .with_level(true)
        .with_thread_names(false)
        .with_thread_ids(false)
        .with_line_number(false)
        .with_file(false)
        .with_target(false)
        .without_time()
        .finish();

    match tracing::subscriber::set_global_default(subscriber) {
        Ok(_) => None,
        Err(_) => Some(Box::new(Error::from("Failed to set global default subscriber")))
    }
}

// Add a helper function to trigger logs with all levels and template
#[no_mangle]
pub extern "C" fn trigger_logs() {
    for level in [LogLevel::Trace, LogLevel::Debug, LogLevel::Info, LogLevel::Warn, LogLevel::Error] {
        match level {
            LogLevel::Trace => trace!("This ia a trace log"),
            LogLevel::Debug => debug!("This is a debug log"),
            LogLevel::Info => info!("This is an info log"),
            LogLevel::Warn => warn!("This is a warn log"),
            LogLevel::Error => error!("This is an error log"),
        }
    }
}

#[no_mangle]
pub extern "C" fn trigger_logs_with_args() {
    let name = "Alice";
    let time = "2025-03-21T10:00:00Z";

    info!(name, time, "User logged in");
    info!("User logged in {} {}", name, time);
    info!(template = "User logged in {} {}", arg1 = name, arg2= time);
    error!(user_id = 42, reason = "timeout", "Login failed");

    // for level in [LogLevel::Trace, LogLevel::Debug, LogLevel::Info, LogLevel::Warn, LogLevel::Error] {
    //     match level {
    //         LogLevel::Trace => trace!("This ia a trace log with args: {}", "arg1"),
    //         LogLevel::Debug => debug!("This is a debug log with args: {}", "arg2"),
    //         LogLevel::Info => info!("This is an info log with args: {}", "arg3"),
    //         LogLevel::Warn => warn!("This is a warn log with args: {}", "arg4"),
    //         LogLevel::Error => error!("This is an error log with args: {}", "arg5"),
    //     }
    // }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Integration test for logger initialization
    #[test]
    fn test_logger_init() {
        // This is more of an integration test and might need to be run in isolation
        // Since it modifies global state (the tracing subscriber)

        let result = ddog_logger_init(LogLevel::Debug, None);
        assert!(result.is_none(), "Logger initialization should succeed");

        // Additional checks could be added to verify the global subscriber is set correctly
        // but would require more complex test infrastructure
    }

    #[test]
    fn test_logger_init_with_callback() {
        extern "C" fn callback(level: LogLevel, template: CharSlice, args: ddcommon_ffi::Vec<CharSlice>) {
            println!("Callback called with level: {:?}, template: {}", level, template);

            // Iterate through the FFIVec to print each argument
            for (i, arg) in args.iter().enumerate() {
                println!("  Arg {}: {}", i, arg);
            }
        }

        let result = ddog_logger_init(LogLevel::Debug, Some(callback));
        assert!(result.is_none(), "Logger initialization should succeed");
    }

    #[test]
    fn test_logger_logs_with_callback() {
        extern "C" fn callback(level: LogLevel, template: CharSlice, args: ddcommon_ffi::Vec<CharSlice>) {
            println!("Callback called with level: {:?}, template: {}", level, template);

            // Iterate through the FFIVec to print each argument
            for (i, arg) in args.iter().enumerate() {
                println!("  Arg {}: {}", i, arg);
            }
        }

        let result = ddog_logger_init(LogLevel::Debug, Some(callback));
        assert!(result.is_none(), "Logger initialization should succeed");

        trigger_logs();
    }

    #[test]
    fn test_logger_logs_with_args_and_callback() {
        extern "C" fn callback(level: LogLevel, template: CharSlice, args: ddcommon_ffi::Vec<CharSlice>) {
            println!("Callback called with level: {:?}, template: {}", level, template);

            // Iterate through the FFIVec to print each argument
            for (i, arg) in args.iter().enumerate() {
                println!("  Arg {}: {}", i, arg);
            }
        }

        let result = ddog_logger_init(LogLevel::Debug, Some(callback));
        assert!(result.is_none(), "Logger initialization should succeed");

        trigger_logs_with_args();
    }
}