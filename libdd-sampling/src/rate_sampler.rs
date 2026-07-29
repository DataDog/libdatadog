// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::constants::{numeric, rate};
use crate::types::TraceIdLike;
use numeric::{KNUTH_FACTOR, MAX_UINT_64BITS};
use std::fmt;

/// Keeps (100 * `sample_rate`)% of the traces randomly.
#[derive(Clone)]
pub struct RateSampler {
    sample_rate: f64,
    sampling_id_threshold: u64,
}

impl fmt::Debug for RateSampler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RateSampler")
            .field("sample_rate", &self.sample_rate)
            .finish()
    }
}

impl RateSampler {
    // Helper method to calculate the threshold from a rate
    fn calculate_threshold(rate: f64) -> u64 {
        if rate >= rate::MAX_SAMPLE_RATE {
            MAX_UINT_64BITS
        } else {
            (rate * (MAX_UINT_64BITS as f64)) as u64
        }
    }

    /// `sample_rate` is clamped between 0.0 and 1.0 inclusive.
    pub fn new(sample_rate: f64) -> Self {
        let clamped_rate = sample_rate.clamp(rate::MIN_SAMPLE_RATE, rate::MAX_SAMPLE_RATE);
        let sampling_id_threshold = Self::calculate_threshold(clamped_rate);

        RateSampler {
            sample_rate: clamped_rate,
            sampling_id_threshold,
        }
    }

    /// Returns the current sample rate
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Determines if a trace should be sampled based on its trace_id and the configured rate.
    /// Returns true if the trace should be kept, false otherwise.
    pub fn sample<T: TraceIdLike>(&self, trace_id: &T) -> bool {
        // Fast-path for sample rate of 0.0 (always drop) or 1.0 (always sample)
        if self.sample_rate <= rate::MIN_SAMPLE_RATE {
            return false;
        }
        if self.sample_rate >= rate::MAX_SAMPLE_RATE {
            return true;
        }

        // Convert trace_id to u128 and then cast to u64 to get the lower 64 bits
        let trace_id_64bits = trace_id.to_u128() as u64;

        let hashed_id = trace_id_64bits.wrapping_mul(KNUTH_FACTOR);

        // If the hashed ID is less than or equal to the threshold, sample the trace
        hashed_id <= self.sampling_id_threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test-only TraceId implementation
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct TestTraceId {
        bytes: [u8; 16],
    }

    impl TestTraceId {
        fn from_bytes(bytes: [u8; 16]) -> Self {
            Self { bytes }
        }

        fn to_bytes(&self) -> [u8; 16] {
            self.bytes
        }
    }

    impl TraceIdLike for TestTraceId {
        fn to_u128(&self) -> u128 {
            u128::from_be_bytes(self.bytes)
        }
    }

    #[test]
    fn check_debug_impl() {
        let sampler = RateSampler::new(0.5);
        let debug_output = format!("{sampler:?}");
        assert!(debug_output.contains("RateSampler"));
        assert!(debug_output.contains("sample_rate: 0.5"));
    }

    #[test]
    fn test_rate_sampler_new() {
        // Standard rates
        let sampler_zero = RateSampler::new(0.0);
        assert_eq!(sampler_zero.sample_rate, 0.0);
        assert_eq!(sampler_zero.sampling_id_threshold, 0);

        let sampler_quarter = RateSampler::new(0.25);
        assert_eq!(sampler_quarter.sample_rate, 0.25);
        assert_eq!(
            sampler_quarter.sampling_id_threshold,
            (0.25 * (MAX_UINT_64BITS as f64)) as u64
        );

        let sampler_half = RateSampler::new(0.5);
        assert_eq!(sampler_half.sample_rate, 0.5);
        assert_eq!(
            sampler_half.sampling_id_threshold,
            (0.5 * (MAX_UINT_64BITS as f64)) as u64
        );

        let sampler_one = RateSampler::new(1.0);
        assert_eq!(sampler_one.sample_rate, 1.0);
        assert_eq!(sampler_one.sampling_id_threshold, MAX_UINT_64BITS);

        // Boundary handling
        let sampler_negative = RateSampler::new(-0.1);
        assert_eq!(sampler_negative.sample_rate, 0.0);

        let sampler_over_one = RateSampler::new(1.1);
        assert_eq!(sampler_over_one.sample_rate, 1.0);
    }

    #[test]
    fn test_rate_sampler_should_sample() {
        // Sample Rate 0.0: Should always return false
        let sampler_zero = RateSampler::new(0.0);
        let mut bytes_zero = [0u8; 16];
        bytes_zero[15] = 1; // Example ID
        let trace_id_zero = TestTraceId::from_bytes(bytes_zero);
        assert!(
            !sampler_zero.sample(&trace_id_zero),
            "sampler_zero should return false"
        );

        // Sample Rate 1.0: Should always return true
        let sampler_one = RateSampler::new(1.0);
        let mut bytes_one = [0u8; 16];
        bytes_one[15] = 2; // Example ID
        let trace_id_one = TestTraceId::from_bytes(bytes_one);
        assert!(
            sampler_one.sample(&trace_id_one),
            "sampler_one should return true"
        );

        // Sample Rate 0.5: Use deterministic cases
        let sampler_half = RateSampler::new(0.5);
        let threshold = sampler_half.sampling_id_threshold;

        // Trace ID that should be sampled (hashed value <= threshold)
        let bytes_sample = [0u8; 16]; // Hashes to 0
        let trace_id_sample = TestTraceId::from_bytes(bytes_sample);
        let sample_u64 = u128::from_be_bytes(trace_id_sample.to_bytes()) as u64;
        let sample_hash = sample_u64.wrapping_mul(KNUTH_FACTOR);
        assert!(sample_hash <= threshold);
        assert!(
            sampler_half.sample(&trace_id_sample),
            "sampler_half should sample trace_id_sample"
        );

        // Trace ID that should be dropped (hashed value > threshold)
        let mut bytes_drop = [0u8; 16];
        bytes_drop[8..16].copy_from_slice(&u64::MAX.to_be_bytes()); // High lower 64 bits
        let trace_id_drop = TestTraceId::from_bytes(bytes_drop);
        let drop_u64 = u128::from_be_bytes(trace_id_drop.to_bytes()) as u64;
        let drop_hash = drop_u64.wrapping_mul(KNUTH_FACTOR);
        // For rate 0.5, threshold is MAX/2. Hashing MAX should result in something > MAX/2
        assert!(
            drop_hash > threshold,
            "Drop hash {drop_hash} should be > threshold {threshold}",
        );
        assert!(
            !sampler_half.sample(&trace_id_drop),
            "sampler_half should drop trace_id_drop"
        );
    }

    #[test]
    fn test_half_rate_sampling() {
        let sampler_half = RateSampler::new(0.5);
        // Trace ID with all zeros hashes to 0, which is always <= threshold for rate > 0
        let bytes_to_sample = [0u8; 16];
        let trace_id_to_sample = TestTraceId::from_bytes(bytes_to_sample);
        assert!(
            sampler_half.sample(&trace_id_to_sample),
            "Sampler with 0.5 rate should sample trace ID 0"
        );
    }
}
