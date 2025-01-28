mod crash_handler;
use crash_handler::install_crash_handler;
use backtrace::{trace_unsynchronized, resolve_frame_unsynchronized};

fn unwind_stack() {
    println!("Unwinding using backtrace-rs...");
    unsafe {
        trace_unsynchronized(|frame| {
            resolve_frame_unsynchronized(frame, |symbol| {
                let mut info = String::new();
                if let Some(name) = symbol.name() {
                    info.push_str(&format!("Function: {}", name));
                }
                if let Some(file) = symbol.filename() {
                    info.push_str(&format!(", File: {:?}", file));
                }
                if let Some(line) = symbol.lineno() {
                    info.push_str(&format!(", Line: {}", line));
                }
                println!("Frame: IP: {:?}, {}", frame.ip(), info);
            });
            true // Continue tracing
        });
    }
}

fn main() {
    install_crash_handler(unwind_stack);

    println!("Running unwind_backtrace example...");
    unsafe {
        *(std::ptr::null_mut() as *mut i32) = 0; // Trigger a crash
    }
}
