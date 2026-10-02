// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_ai_usage::{
    context_tokens_bucket, normalize, valid_label, Details, InputBasis, Issue, Measurement,
    ToolCount, UsageInput, Value, MAX_COUNT,
};
use Value::{Integer as I, Missing};

fn example() -> UsageInput {
    UsageInput {
        input: I(1000),
        output: I(120),
        cache_read: I(400),
        cache_write: I(300),
        cache_write_5m: I(100),
        cache_write_1h: I(200),
        output_details: Details {
            reasoning_tokens: I(70),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn cache_and_reasoning_are_not_additive_twice() {
    let result = normalize(Some(&example()));
    assert!(result.issues.is_empty());
    assert_eq!(result.quantities.values().sum::<u64>(), 1120);
    assert_eq!(result.quantities["input_uncached_tokens"], 300);
    assert_eq!(
        result.observations["reasoning_output_tokens"],
        Measurement::Count(70)
    );
}

#[test]
fn native_and_normalized_input_agree() {
    let mut native = example();
    native.input = I(300);
    native.input_basis = InputBasis::ExcludesCache;
    assert_eq!(normalize(Some(&native)), normalize(Some(&example())));
}

#[test]
fn missing_and_explicit_zero_differ() {
    let missing = normalize(None);
    assert!(missing.issues.contains(&Issue::MissingUsage));
    assert!(missing.quantities.is_empty());
    let incomplete = normalize(Some(&UsageInput {
        input: I(0),
        output: I(0),
        ..Default::default()
    }));
    assert!(!incomplete.quantities.contains_key("input_uncached_tokens"));
    assert!(incomplete.issues.contains(&Issue::CacheReadDetailMissing));
    assert!(incomplete.issues.contains(&Issue::CacheWriteDetailMissing));
    let complete = normalize(Some(&UsageInput {
        input: I(0),
        output: I(0),
        cache_read: I(0),
        cache_write: I(0),
        ..Default::default()
    }));
    assert_eq!(complete.quantities["input_uncached_tokens"], 0);
    assert!(complete.issues.is_empty());
}

#[test]
fn unknown_ttl_is_not_assumed_to_be_five_minutes() {
    let mut input = example();
    input.cache_write_5m = Missing;
    input.cache_write_1h = Missing;
    let result = normalize(Some(&input));
    assert_eq!(
        result.quantities["input_cache_write_unknown_ttl_tokens"],
        300
    );
    assert_eq!(result.quantities["input_cache_write_5m_tokens"], 0);
    assert!(result.issues.contains(&Issue::CacheWriteTtlUnknown));
}

#[test]
fn rejects_inconsistent_totals_and_ttl() {
    for (input, expected) in [
        (
            UsageInput {
                cache_read: I(1001),
                ..example()
            },
            Issue::InconsistentUsage,
        ),
        (
            UsageInput {
                cache_write_1h: I(201),
                ..example()
            },
            Issue::InconsistentCacheTtl,
        ),
        (
            UsageInput {
                output: I(69),
                ..example()
            },
            Issue::InconsistentUsage,
        ),
    ] {
        let result = normalize(Some(&input));
        assert!(result.issues.contains(&expected));
        assert!(result.quantities.is_empty());
    }
}

#[test]
fn rejects_invalid_counts_and_overflow_without_panicking() {
    for value in [
        Value::Invalid,
        Value::Fraction(1.0),
        Value::Fraction(f64::NAN),
        I(MAX_COUNT + 1),
        I(u64::MAX),
    ] {
        let result = normalize(Some(&UsageInput {
            input: value,
            ..example()
        }));
        assert!(result.issues.contains(&Issue::InvalidUsage));
        assert!(result.quantities.is_empty());
    }
    let result = normalize(Some(&UsageInput {
        input: I(MAX_COUNT),
        input_basis: InputBasis::ExcludesCache,
        ..example()
    }));
    assert!(result.issues.contains(&Issue::InvalidUsage));
}

#[test]
fn integer_observations_retain_exact_maximum() {
    let result = normalize(Some(&UsageInput {
        input: I(MAX_COUNT),
        output: I(0),
        cache_read: I(0),
        cache_write: I(0),
        ..Default::default()
    }));
    assert_eq!(
        result.observations["context_tokens"],
        Measurement::Count(MAX_COUNT)
    );
    assert_eq!(result.quantities["input_uncached_tokens"], MAX_COUNT);
}

#[test]
fn embeddings_do_not_require_output() {
    let input = UsageInput {
        input: I(123),
        embedding: true,
        ..Default::default()
    };
    let result = normalize(Some(&input));
    assert_eq!(result.quantities["input_uncached_tokens"], 123);
    assert!(!result.quantities.contains_key("output_tokens"));
    assert!(result.issues.is_empty());
    let result = normalize(Some(&UsageInput {
        embedding: false,
        ..input
    }));
    assert!(result.issues.contains(&Issue::UnsupportedUsageShape));
}

#[test]
fn ambiguous_multimodal_keeps_fractional_observations_not_partitions() {
    let result = normalize(Some(&UsageInput {
        input_details: Details {
            audio_tokens: I(20),
            audio_length_seconds: Value::Fraction(2.5),
            video_length_seconds: Value::Fraction(1.25),
            image_count: I(2),
            ..Default::default()
        },
        ..example()
    }));
    assert!(result.quantities.is_empty());
    assert!(result
        .issues
        .contains(&Issue::MultimodalPartitionUnsupported));
    assert_eq!(
        result.observations["input_audio_length_seconds"],
        Measurement::Duration(2.5)
    );
    assert_eq!(
        result.observations["input_video_length_seconds"],
        Measurement::Duration(1.25)
    );
}

#[test]
fn rejects_bad_durations() {
    for value in [
        Value::Fraction(f64::NAN),
        Value::Fraction(f64::INFINITY),
        Value::Fraction(-0.1),
        I(MAX_COUNT + 1),
        Value::Invalid,
    ] {
        let result = normalize(Some(&UsageInput {
            input_details: Details {
                audio_length_seconds: value,
                ..Default::default()
            },
            ..example()
        }));
        assert!(result.issues.contains(&Issue::InvalidUsage));
        assert!(!result
            .observations
            .contains_key("input_audio_length_seconds"));
    }
}

#[test]
fn tools_are_counted_once_and_conflicts_retained() {
    let mut input = example();
    input.web_search_requests = ToolCount {
        native: I(2),
        normalized: I(2),
    };
    input.browser_open_requests = ToolCount {
        native: I(3),
        normalized: Missing,
    };
    let result = normalize(Some(&input));
    assert_eq!(result.quantities["web_search_requests"], 2);
    assert_eq!(result.quantities["browser_open_requests"], 3);
    input.web_search_requests.normalized = I(3);
    let result = normalize(Some(&input));
    assert!(result.issues.contains(&Issue::ConflictingToolUsage));
    assert!(!result.quantities.contains_key("web_search_requests"));
    assert!(!result.quantities.contains_key("browser_open_requests"));
    assert_eq!(
        result.observations["web_search_requests"],
        Measurement::Count(2)
    );
}

#[test]
fn every_global_bucket_boundary_and_larger_contexts() {
    let mut boundaries: Vec<u64> = (0..40).map(|n| 32_000 * (1 << n)).collect();
    boundaries.extend([200_000, 272_000]);
    boundaries.sort_unstable();
    let mut lower = 0;
    for upper in boundaries {
        for token in [lower, upper - 1, upper] {
            if token <= MAX_COUNT {
                assert_eq!(
                    context_tokens_bucket(I(token), true),
                    format!("{lower}_{upper}")
                );
            }
        }
        if lower <= MAX_COUNT && MAX_COUNT <= upper {
            assert_eq!(
                context_tokens_bucket(I(MAX_COUNT), true),
                format!("{lower}_{upper}")
            );
        }
        lower = upper + 1;
    }
    for value in [
        Missing,
        Value::Invalid,
        I(MAX_COUNT + 1),
        Value::Fraction(32_000.0),
    ] {
        assert_eq!(context_tokens_bucket(value, true), "unknown");
    }
    assert_eq!(context_tokens_bucket(I(1), false), "unknown");
}

#[test]
fn labels_preserve_unicode_without_accepting_secret_like_values() {
    for value in [
        "",
        " ",
        " a",
        "a\n",
        "a\u{7f}",
        "Sk-secret",
        "BEARER token",
        "a\u{a0}",
        "\u{1c}a",
    ] {
        assert!(!valid_label(value, 256), "{value:?}");
    }
    assert!(valid_label("你好", 2));
    assert!(!valid_label("你好", 1));
    assert!(valid_label("apikey_public_id", 256));
    assert!(!valid_label("id", 0));
}

#[test]
fn conservation_over_many_partitions_and_bases() {
    // Deterministic, dependency-free property check across 10,000 cache partitions.
    let mut seed = 19_u64;
    for _ in 0..10_000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let prompt = seed % (MAX_COUNT + 1);
        let read = (seed / 11) % (prompt + 1);
        let write = (seed / 17) % (prompt - read + 1);
        let five = write / 3;
        let hour = write / 5;
        let output = seed % 100_000;
        let input = UsageInput {
            input: I(prompt),
            output: I(output),
            cache_read: I(read),
            cache_write: I(write),
            cache_write_5m: I(five),
            cache_write_1h: I(hour),
            ..Default::default()
        };
        let result = normalize(Some(&input));
        assert_eq!(result.quantities.values().sum::<u64>(), prompt + output);
        let native = UsageInput {
            input: I(prompt - read - write),
            input_basis: InputBasis::ExcludesCache,
            ..input
        };
        assert_eq!(result, normalize(Some(&native)));
    }
}
