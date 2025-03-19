use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::Mutex;
use log::{Log, Record, Level, Metadata};
use ddcommon_ffi::slice::{AsBytes, CharSlice};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use ddcommon_ffi::Error;

#[repr(C)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum LogLevel {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
    Trace = 4,
}

type LogCallback = extern "C" fn(level: LogLevel, message: CharSlice);

static LOGGER: FfiLogger = FfiLogger { callback: Mutex::new(None) };

struct FfiLogger {
    callback: Mutex<Option<LogCallback>>,
}

impl Log for FfiLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let level = match record.level() {
            Level::Error => LogLevel::Error,
            Level::Warn  => LogLevel::Warn,
            Level::Info  => LogLevel::Info,
            Level::Debug => LogLevel::Debug,
            Level::Trace => LogLevel::Trace
        };

        let message = record.args().to_string();
        let slice = CharSlice::from(message.as_str());

        if let Some(cb) = *self.callback.lock().unwrap() {
            cb(level, slice);
        }
    }

    fn flush(&self) {}
}

#[no_mangle]
pub extern "C" fn ddog_ffi_logger_set_log_callback<'a>(callback: LogCallback) -> Option<Box<Error>> {
    let mut cb = match LOGGER.callback.lock() {
        Ok(guard) => guard,
        Err(_) => {
            let error = Error::from("Failed to acquire lock for logger callback");
            return Some(Box::new(error));
        }
    };
    
    *cb = Some(callback);

    match log::set_logger(&LOGGER) {
        Ok(_) => {
            log::set_max_level(log::LevelFilter::Error);
            println!("✅ Rust logger initialized successfully!");
            None
        }
        Err(e) => {
            println!("❗ Logger was already set!");
            let message = e.to_string();
            let error = Error::from(message);
            Some(Box::new(error))
        }
    }
}

#[no_mangle]
pub extern "C" fn trigger_logs_with_message(level: LogLevel, message: CharSlice) {
    println!("🚀 trigger_logs_with_message() called with level {:?}, while max level is set to {:?}", level, log::max_level());
    match level {
        LogLevel::Error => log::error!("{}", message.to_utf8_lossy()),
        LogLevel::Warn => log::warn!("{}", message.to_utf8_lossy()),
        LogLevel::Info => log::info!("{}", message.to_utf8_lossy()),
        LogLevel::Debug => log::debug!("{}", message.to_utf8_lossy()),
        LogLevel::Trace => log::trace!("{}", message.to_utf8_lossy()),
    }
}

/// Sets the maximum log level for the logger
#[no_mangle]
pub extern "C" fn ddog_ffi_logger_set_max_log_level(level: LogLevel) {
    let filter = match level {
        LogLevel::Error => log::LevelFilter::Error,
        LogLevel::Warn => log::LevelFilter::Warn,
        LogLevel::Info => log::LevelFilter::Info,
        LogLevel::Debug => log::LevelFilter::Debug,
        LogLevel::Trace => log::LevelFilter::Trace,
    };
    log::set_max_level(filter);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use log;
    use std::sync::Once;

    static INIT: Once = Once::new();
    static CALLED: AtomicBool = AtomicBool::new(false);
    static RECEIVED_LEVEL: Mutex<Option<LogLevel>> = Mutex::new(None);
    static RECEIVED_MESSAGE: Mutex<Option<String>> = Mutex::new(None);

    extern "C" fn test_callback(level: LogLevel, message: CharSlice) {
        let msg = message.to_utf8_lossy().to_string();
        println!("Received log from Rust: {} with level {:?}", msg, level);

        // Store received values for verification
        let _ = RECEIVED_LEVEL.lock().map(|mut guard| *guard = Some(level));
        let _ = RECEIVED_MESSAGE.lock().map(|mut guard| *guard = Some(msg));
        CALLED.store(true, Ordering::SeqCst);
    }

    fn reset_test_state() {
        CALLED.store(false, Ordering::SeqCst);
        let _ = RECEIVED_LEVEL.lock().map(|mut guard| *guard = None);
        let _ = RECEIVED_MESSAGE.lock().map(|mut guard| *guard = None);
    }

    fn get_received_message() -> Option<String> {
        RECEIVED_MESSAGE.lock().ok().and_then(|guard| guard.clone())
    }

    fn get_received_level() -> Option<LogLevel> {
        RECEIVED_LEVEL.lock().ok().and_then(|guard| guard.clone())
    }

    #[test]
    fn test_logger_functionality() {
        let result = ddog_ffi_logger_set_log_callback(test_callback);
        match result {
            Some(err) => panic!("Logger setup should succeed, got error: {}", err),
            None => println!("✅ Logger initialized successfully"),
        }

        // 1. Test error handling for second initialization
        println!("\n🧪 Testing logger error handling...");
        {
            reset_test_state();
            let error = ddog_ffi_logger_set_log_callback(test_callback);
            match error {
                None => panic!("Second logger setup should return error"),
                Some(error) => {
                    let error_msg = error.to_string();
                    println!("Got expected error message: {}", error_msg);
                    assert!(error_msg.contains("logger"), "Error message should mention logger");
                }
            }
        }

        // 2. Test basic message logging with different levels
        println!("\n🧪 Testing basic message logging...");
        {
            // Set log level to TRACE to allow all logs
            ddog_ffi_logger_set_max_log_level(LogLevel::Trace);
            println!("Set max log level to TRACE");

            let test_cases = vec![
                (LogLevel::Error, "Custom error message"),
                (LogLevel::Warn, "Custom warning message"),
                (LogLevel::Info, "Custom info message"),
                (LogLevel::Debug, "Custom debug message"),
                (LogLevel::Trace, "Custom trace message"),
            ];

            for (level, message) in test_cases {
                reset_test_state();
                let slice = CharSlice::from(message);
                trigger_logs_with_message(level, slice);

                assert!(CALLED.load(Ordering::SeqCst), 
                    "Log callback was not triggered for level {:?}", level);
                assert_eq!(
                    get_received_level(),
                    Some(level),
                    "Incorrect level received for {:?}", level
                );
                assert_eq!(
                    get_received_message(),
                    Some(message.to_string()),
                    "Incorrect message received for level {:?}", level
                );
            }
        }

        // 3. Test Unicode message handling
        println!("\n🧪 Testing Unicode message handling...");
        {
            reset_test_state();
            // Ensure appropriate log level is set
            ddog_ffi_logger_set_max_log_level(LogLevel::Info);
            
            let unicode_message = "🦀 Rust loves 유니코드 and Unicode! 🎉";
            let slice = CharSlice::from(unicode_message);
            trigger_logs_with_message(LogLevel::Info, slice);

            assert!(CALLED.load(Ordering::SeqCst), "Log callback was not triggered");
            assert_eq!(
                get_received_message(),
                Some(unicode_message.to_string())
            );
            assert_eq!(
                get_received_level(),
                Some(LogLevel::Info)
            );
        }

        // 4. Test max log level filtering
        println!("\n🧪 Testing max log level filtering...");
        {
            let test_cases = vec![
                // When max level is ERROR
                (LogLevel::Error, LogLevel::Error, "Error message", true),
                (LogLevel::Error, LogLevel::Warn, "Warning message", false),
                
                // When max level is INFO
                (LogLevel::Info, LogLevel::Error, "Error message", true),
                (LogLevel::Info, LogLevel::Info, "Info message", true),
                (LogLevel::Info, LogLevel::Debug, "Debug message", false),
                
                // When max level is TRACE
                (LogLevel::Trace, LogLevel::Debug, "Debug message", true),
                (LogLevel::Trace, LogLevel::Trace, "Trace message", true),
            ];

            for (max_level, log_level, message, should_log) in test_cases {
                reset_test_state();
                ddog_ffi_logger_set_max_log_level(max_level);
                println!("Testing with max_level {:?}, log_level {:?}", max_level, log_level);
                
                let slice = CharSlice::from(message);
                trigger_logs_with_message(log_level, slice);

                assert_eq!(
                    CALLED.load(Ordering::SeqCst),
                    should_log,
                    "Unexpected logging behavior for max_level {:?} with log_level {:?}",
                    max_level,
                    log_level
                );

                if should_log {
                    assert_eq!(
                        get_received_level(),
                        Some(log_level),
                        "Incorrect level received for max_level {:?} with log_level {:?}",
                        max_level,
                        log_level
                    );
                    assert_eq!(
                        get_received_message(),
                        Some(message.to_string()),
                        "Incorrect message received for max_level {:?} with log_level {:?}",
                        max_level,
                        log_level
                    );
                }
            }
        }

        println!("\n✅ All tests completed successfully!");
    }
}