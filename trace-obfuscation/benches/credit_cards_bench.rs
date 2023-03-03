use criterion::Throughput::Elements;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use datadog_trace_obfuscation::credit_cards::is_card_number;

fn is_card_number_bench(c: &mut Criterion) {
    let ccs = vec![
        "378282246310005",
        "  378282246310005",
        "  3782-8224-6310-005 ",
        "37828224631000521389798", // valid but too long
        "37828224631",             // valid but too short
        "x371413321323331",        // invalid characters
        "",
    ];
    let mut group = c.benchmark_group("is_card_number");
    for c in ccs.iter() {
        group.throughput(Elements(1));
        group.bench_with_input(BenchmarkId::new("is_card_number", c), c, |b, i| {
            b.iter(|| is_card_number(i, true))
        });
    }
}

fn is_card_number_no_luhn_bench(c: &mut Criterion) {
    let ccs = vec![
        "378282246310005",
        "  378282246310005",
        "  3782-8224-6310-005 ",
        "37828224631000521389798", // valid but too long
        "37828224631",             // valid but too short
        "x371413321323331",        // invalid characters
        "",
    ];
    let mut group = c.benchmark_group("is_card_number_no_luhn");
    for c in ccs.iter() {
        group.throughput(Elements(1));
        group.bench_with_input(BenchmarkId::new("is_card_number_no_luhn", c), c, |b, i| {
            b.iter(|| is_card_number(i, false))
        });
    }
}

criterion_group!(benches, is_card_number_bench, is_card_number_no_luhn_bench);
criterion_main!(benches);
