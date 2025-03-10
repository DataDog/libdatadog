use std::ffi::{CString, CStr};
use std::os::raw::{c_char, c_int};
use std::sync::Mutex;
use log::{Log, Record, Level, Metadata};

/// Type definition for the callback to C#
type LogCallback = extern "C" fn(level: c_int, message: *const c_char);

/// Global logger instance
static LOGGER: FfiLogger = FfiLogger { callback: Mutex::new(None) };

/// Custom logger that forwards logs to C#
struct FfiLogger {
    callback: Mutex<Option<LogCallback>>,
}

impl Log for FfiLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.lewvel() <= Level::Error
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let level = match record.level() {
            Level::Error => 3,
            Level::Warn  => 2,
            Level::Info  => 1,
            Level::Debug => 0,
            Level::Trace => 0,
        };

        let message = CString::new(record.args().to_string()).expect("CString failed");
        let message_ptr = message.as_ptr(); // Don't free, keep in memory

        if let Some(cb) = *self.callback.lock().unwrap() {
            cb(level, message_ptr);
        }
    }

    fn flush(&self) {}
}

/// Registers a callback for logs in Rust
#[no_mangle]
pub extern "C" fn set_log_callback(callback: LogCallback) -> i32 {
    let mut cb = LOGGER.callback.lock().unwrap();
    *cb = Some(callback);

    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Error);
        println!("✅ Rust logger initialized successfully!");
        0
    } else {
        println!("❗ Logger was already set!");
        1
    }
}

/// Function to trigger logs (for testing)
#[no_mangle]
pub extern "C" fn trigger_logs() {
    std::panic::catch_unwind(|| {
        println!("🚀 trigger_logs() called");
        log::error!("🔥 This is an error message from Rust!");
        log::warn!("⚠️ This is a warning from Rust!");
    }).ok(); // Silently discard panic (prevents process crash)
}

/// Function to free heap-allocated memory in C#
#[no_mangle]
pub extern "C" fn free_log_message(message: *mut c_char) {
    if message.is_null() {
        println!("⚠️ Warning: Attempted to free NULL pointer.");
        return;
    }
    unsafe {
        println!("🔄 Freeing memory at {:?}", message);
        let _ = CString::from_raw(message as *mut c_char);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::ffi::{CStr, c_char, c_int};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use log::LevelFilter;

    static CALLED: AtomicBool = AtomicBool::new(false);
    static LOGGER_INIT: Mutex<()> = Mutex::new(());

    extern "C" fn callback(level: c_int, message: *mut c_char) {
        let msg = unsafe { CStr::from_ptr(message).to_string_lossy() };
        println!("Received log from Rust: {}", msg); // Debug output

        // Check log content
        assert_eq!(msg, "🔥 This is an error message from Rust!");
        assert_eq!(level, 3);

        // Mark that the callback was triggered
        CALLED.store(true, Ordering::SeqCst);

        // Free the message
        free_log_message(message);
    }

    #[test]
    fn test_set_log_callback() {
        let _lock = LOGGER_INIT.lock().unwrap(); // Ensure the test runs serially

        let result = set_log_callback(callback);
        assert_eq!(result, 0, "Failed to set log callback");

        trigger_logs();

        // Ensure the log callback was called
        assert!(CALLED.load(Ordering::SeqCst), "Log callback was not triggered");
    }

}