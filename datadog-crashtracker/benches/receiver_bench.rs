// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{black_box, criterion_group, BenchmarkId, Criterion};
use datadog_crashtracker::benchmark::receiver_entry_point;
use datadog_crashtracker::shared::constants::*;
use datadog_crashtracker::{
    default_signals, get_data_folder_path, CrashtrackerConfiguration, SharedLibrary,
    StacktraceCollection,
};
use std::fmt::Write;
use std::time::Duration;
use tokio::io::BufReader;

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
    writeln!(report, "{DD_CRASHTRACK_BEGIN_PROCINFO}")
        .expect("Failed to write DD_CRASHTRACK_BEGIN_PROCINFO");
    writeln!(report, "{{ \"pid\": {} }}", std::process::id()).expect("Failed to write PID");
    writeln!(report, "{DD_CRASHTRACK_END_PROCINFO}")
        .expect("Failed to write DD_CRASHTRACK_END_PROCINFO");
}

fn add_config(report: &mut String) {
    writeln!(report, "{DD_CRASHTRACK_BEGIN_CONFIG}")
        .expect("Failed to write DD_CRASHTRACK_BEGIN_CONFIG");
    let config = CrashtrackerConfiguration::new(
        vec![], // additional_files
        true,   // create_alt_stack
        true,   // use_alt_stack
        None,
        StacktraceCollection::EnabledWithSymbolsInReceiver,
        default_signals(),
        Some(Duration::from_secs(10)),
        Some("".to_string()), // unix_socket_path
        true,                 // demangle_names
    )
    .expect("Failed to create crashtracker configuration");
    let config_str =
        serde_json::to_string(&config).expect("Failed to serialize crashtracker configuration");
    writeln!(report, "{config_str}").expect("Failed to write crashtracker configuration");
    writeln!(report, "{DD_CRASHTRACK_END_CONFIG}")
        .expect("Failed to write DD_CRASHTRACK_END_CONFIG");
}

fn add_stacktrace(report: &mut String, test_cpp_so: &SharedLibrary, test_c_so: &SharedLibrary) {
    writeln!(report, "{DD_CRASHTRACK_BEGIN_STACKTRACE}")
        .expect("Failed to write DD_CRASHTRACK_BEGIN_STACKTRACE");

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

    writeln!(report, "{DD_CRASHTRACK_END_STACKTRACE}")
        .expect("Failed to write DD_CRASHTRACK_END_STACKTRACE");
}

fn create_crash_report(test_cpp_so: &SharedLibrary, test_c_so: &SharedLibrary) -> String {
    // Manual test revealed that the report size was arount 3000 bytes
    let mut report = String::with_capacity(3000);
    add_proc_info(&mut report);
    add_config(&mut report);
    add_stacktrace(&mut report, test_cpp_so, test_c_so);
    writeln!(report, "{DD_CRASHTRACK_DONE}").expect("Failed to write DD_CRASHTRACK_DONE");
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
        .expect("Failed to get the data folder path")
        .join("libtest.so")
        .canonicalize()
        .unwrap();

    let sofile_cpp_path = get_data_folder_path()
        .expect("Failed to get the data folder path")
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
