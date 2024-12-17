// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use criterion::{black_box, criterion_group, Criterion};
use datadog_trace_obfuscation::ip_address;
use std::borrow::Cow;

fn quantize_peer_ip_address_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("ip_address");
    let cases = [
        "http://172.24.160.151:8091,172.24.163.33:8091,172.24.164.111:8091,172.24.165.203:8091,172.24.168.235:8091,172.24.170.130:8091",
        "10-60-160-172.my-service.namespace.svc.abc.cluster.local",
        "ip-10-152-4-129.ec2.internal",
        "192.168.1.1:1234,10.23.1.1:53,10.23.1.1,fe80::1ff:fe23:4567:890a,[fe80::1ff:fe23:4567:890a]:8080,foo.dog",
        "not-an-ip.foo.bar, still::not::an::ip, 1.2.3.is.not.an.ip, 12:12:12:12:12:12:12:12:12:12:12:12:12",
    ];

    group.bench_function("quantize_peer_ip_address_benchmark", |b| {
        b.iter_batched_ref(
            // Keep the String instances around to avoid measuring the deallocation cost
            || Vec::with_capacity(cases.len()) as Vec<Cow<'_, str>>,
            |res: &mut Vec<Cow<'_, str>>| {
                for c in cases {
                    res.push(black_box(ip_address::quantize_peer_ip_addresses(c)));
                }
            },
            criterion::BatchSize::LargeInput,
        )
    });
}

criterion_group!(benches, quantize_peer_ip_address_benchmark);
