// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{black_box, criterion_group, BenchmarkId, Criterion};
use datadog_crashtracker::benchmark::receiver_entry_point;
use libc::c_void;
use std::ffi::CString;
use std::fmt::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::BufReader;

struct SharedLibrary {
    handle: *mut c_void,
}

impl SharedLibrary {
    fn open(lib_path: &str) -> Result<Self, String> {
        let cstr = CString::new(lib_path).map_err(|e| e.to_string())?;
        // Use RTLD_NOW or another flag
        let handle = unsafe { libc::dlopen(cstr.as_ptr(), libc::RTLD_NOW) };
        if handle.is_null() {
            Err("Failed to open library".to_string())
        } else {
            Ok(Self { handle })
        }
    }

    fn get_symbol_address(&self, symbol: &str) -> Result<String, String> {
        let cstr = CString::new(symbol).map_err(|e| e.to_string())?;
        let sym = unsafe { libc::dlsym(self.handle, cstr.as_ptr()) };
        if sym.is_null() {
            Err(format!("Failed to find symbol: {}", symbol))
        } else {
            Ok(format!("{:p}", sym))
        }
    }
}

impl Drop for SharedLibrary {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { libc::dlclose(self.handle) };
        }
    }
}

fn get_data_folder_path() -> PathBuf {
    Path::new(&env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .canonicalize()
        .expect("Failed to canonicalize base path for libtest")
}

macro_rules! add_frame {
    ($report:expr, $fn:expr, $lib:expr) => {
        add_frame!($report, $lib.get_symbol_address($fn).unwrap().as_str());
    };
    ($report:expr, $address:expr) => {
        writeln!(
            $report,
            "{{ \"ip\": \"{}\", \"module_address\": \"0x21\", \"sp\": \"0x11\" }}",
            $address
        )
        .expect("Failed to write frame");
    };
}

fn add_proc_info(report: &mut String) {
    report.push_str("DD_CRASHTRACK_BEGIN_PROCESSINFO\n");
    writeln!(report, "{{ \"pid\": {} }}", std::process::id()).expect("Failed to write PID");
    report.push_str("DD_CRASHTRACK_END_PROCESSINFO\n");
}

fn add_config(report: &mut String) {
    report.push_str("DD_CRASHTRACK_BEGIN_CONFIG\n");
    report.push_str("{\"additional_files\":[],\"create_alt_stack\":true,\"demangle_names\":true,\"endpoint\":null,\"resolve_frames\":\"EnabledWithSymbolsInReceiver\",\"signals\":[4,6,7,11],\"timeout\":{\"secs\":10,\"nanos\":0},\"unix_socket_path\":\"\",\"use_alt_stack\":true}\n");
    report.push_str("DD_CRASHTRACK_END_CONFIG\n");
}

fn add_stacktrace(report: &mut String, test_cpp_so: &SharedLibrary, test_c_so: &SharedLibrary) {
    report.push_str("DD_CRASHTRACK_BEGIN_STACKTRACE\n");

    add_frame!(report, "my_function", test_c_so);
    add_frame!(report, "func1", test_c_so);
    add_frame!(report, "func2", test_c_so);
    add_frame!(report, "0x01");
    add_frame!(report, "0x02");
    add_frame!(report, "_Z12cpp_functionv", test_cpp_so);
    add_frame!(
        report,
        "_ZN10FirstClass10InnerClass12InnerMethod1Ev",
        test_cpp_so
    );
    add_frame!(
        report,
        "_ZN10FirstClass10InnerClass12InnerMethod2Ev",
        test_cpp_so
    );
    add_frame!(report, "0x03");
    add_frame!(report, "0x03");
    add_frame!(report, "0x05");
    add_frame!(report, "func3", test_c_so);
    add_frame!(report, "_ZN10FirstClass7Method1Ev", test_cpp_so);
    add_frame!(report, "func4", test_c_so);
    add_frame!(
        report,
        "_ZN10FirstClass7Method2EibNSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEEE",
        test_cpp_so
    );
    add_frame!(report, "0x06");
    add_frame!(report, "func5", test_c_so);
    add_frame!(report, "func6", test_c_so);
    add_frame!(
        report,
        "_ZN11MyNamespace16ClassInNamespace18MethodInNamespace1Efx",
        test_cpp_so
    );
    add_frame!(report, "0x07");
    add_frame!(
        report,
        "_ZN11MyNamespace16ClassInNamespace18MethodInNamespace2Edc",
        test_cpp_so
    );
    add_frame!(report, "0x08");
    add_frame!(report, "func6", test_c_so);
    add_frame!(report, "0x09");
    add_frame!(
        report,
        "_ZN11MyNamespace16ClassInNamespace21InnerClassInNamespace12InnerMethod1Ev",
        test_cpp_so
    );
    add_frame!(report, "0x0A");
    add_frame!(
        report,
        "_ZN11MyNamespace16ClassInNamespace21InnerClassInNamespace12InnerMethod2Eiix",
        test_cpp_so
    );
    add_frame!(report, "0x0B");
    add_frame!(report, "func7", test_c_so);
    add_frame!(report, "func8", test_c_so);
    add_frame!(
        report,
        "_ZN11MyNamespace16ClassInNamespace21InnerClassInNamespace12InnerMethod2Eiix",
        test_cpp_so
    );
    add_frame!(report, "func9", test_c_so);
    add_frame!(report, "func10", test_c_so);
    add_frame!(report, "0x00");

    report.push_str("DD_CRASHTRACK_END_STACKTRACE\n");
}

fn create_crash_report(test_cpp_so: &SharedLibrary, test_c_so: &SharedLibrary) -> String {
    // Manual test revealed that the report size was arount 3000 bytes
    let mut report = String::with_capacity(3000);
    add_proc_info(&mut report);
    add_config(&mut report);
    add_stacktrace(&mut report, test_cpp_so, test_c_so);
    report.push_str("DD_CRASHTRACK_DONE\n");
    report
}

async fn bench_receiver_entry_point_from_str(data: &str) {
    let cursor = std::io::Cursor::new(data.as_bytes());
    let reader = BufReader::new(cursor);
    let timeout = Duration::from_millis(5000);

    let _ = receiver_entry_point(timeout, reader).await;
}

fn load_test_libraries() -> (SharedLibrary, SharedLibrary) {
    let sofile_c_path = get_data_folder_path()
        .join("libtest.so")
        .canonicalize()
        .unwrap();

    let sofile_cpp_path = get_data_folder_path()
        .join("libtest_cpp.so")
        .canonicalize()
        .unwrap();

    let test_c_so = SharedLibrary::open(sofile_c_path.to_str().unwrap()).unwrap();
    let test_cpp_so = SharedLibrary::open(sofile_cpp_path.to_str().unwrap()).unwrap();
    (test_cpp_so, test_c_so)
}

pub fn receiver_entry_point_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("receiver_entry_point");

    // the libraries must be opened as long as the benchmark is running
    // That why we pass them as references
    let (sofile_cpp, sofile_c) = load_test_libraries();
    let report = create_crash_report(&sofile_cpp, &sofile_c);
    group.bench_with_input(
        BenchmarkId::new("report", report.len()),
        &report,
        |b, data| {
            b.iter(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(bench_receiver_entry_point_from_str(black_box(data)))
            });
        },
    );
}

criterion_group!(benches, receiver_entry_point_benchmarks);
