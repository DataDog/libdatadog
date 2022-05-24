// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use ddcommon::tag::Tag;
use ddtelemetry::{data, worker};

macro_rules! timeit {
    ($op_name:literal, $op:block) => {{
        let start = std::time::Instant::now();
        let res = $op;
        let delta = std::time::Instant::now().duration_since(start);
        println!(
            concat!($op_name, " took {} ms"),
            delta.as_secs_f64() * 1000.0
        );
        res
    }};
}

fn main() {
    let handle = worker::TelemetryWorkerBuilder::new(
        "paul-mac".into(),
        "test_rust".into(),
        "rust".into(),
        "1.56".into(),
        "none".into(),
    )
    .run();

    let test_telemetry_ping_metric = handle.register_metric_context(
        "test_telemetry.ping".into(),
        Vec::new(),
        data::metrics::MetricType::Count,
        false,
        data::metrics::MetricNamespace::Trace,
    );
    handle.send_start().unwrap();

    handle
        .add_point(1.0, &test_telemetry_ping_metric, Vec::new())
        .unwrap();

    let tags = vec![Tag::from_value("foo:bar").unwrap()];
    handle
        .add_point(2.0, &test_telemetry_ping_metric, tags)
        .unwrap();

    timeit!("sleep", {
        std::thread::sleep(std::time::Duration::from_secs(20));
    });

    handle
        .add_log(
            "init.log",
            "Hello there!".into(),
            data::LogLevel::Debug,
            None,
        )
        .unwrap();
    handle
        .add_log(
            "init.log",
            "Another log, with the same logging identifier".into(),
            data::LogLevel::Debug,
            None,
        )
        .unwrap();
    handle
        .add_log(
            "exception.log",
            "Something really bad happened".into(),
            data::LogLevel::Error,
            Some("At line 56".into()),
        )
        .unwrap();

    handle
        .add_point(2.0, &test_telemetry_ping_metric, Vec::new())
        .unwrap();

    // About 200ms (the time it takes to send a app-closing request)
    timeit!("shutdown", {
        handle.send_stop().unwrap();
        handle.wait_for_shutdown();
    });
}
