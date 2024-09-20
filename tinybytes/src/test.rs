// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use core::str;
use std::sync::atomic::{self, AtomicUsize};

use super::*;
use once_cell::sync::OnceCell;
use pretty_assertions::assert_eq;
use proptest::test_runner;
use test_case::test_case;

static HELLO_BYTES: OnceCell<Bytes> = OnceCell::new();

fn hello() -> Bytes {
    let bytes = HELLO_BYTES.get_or_init(|| Bytes::copy_from_slice(b"hello"));
    bytes.clone()
}

fn hello_slice(range: impl RangeBounds<usize>) -> Bytes {
    hello().slice(range)
}

#[allow(clippy::reversed_empty_ranges)]
#[test_case(0..0, ""; "0 to 0 is empty")]
#[test_case(.., "hello"; "full range is hello")]
#[test_case(1..3, "el"; "1 to 3 is el")]
#[test_case(1..=3, "ell"; "1 to 3 inclusive is ell")]
#[test_case(..3, "hel"; "start to 3 is hel")]
#[test_case(3.., "lo"; "3 to end is lo")]
#[test_case(0..5, "hello"; "0 to 5 is hello")]
#[test_case(0.., "hello"; "0 to end is hello")]
#[test_case(0..=5, "unused" => panics "range end must not be greater than length: 6 > 5"; "0 to 5 inclusive")]
#[test_case(4..3, "unused" => panics "range start must not be greater than end: 4 > 3"; "4 to 3")]
#[test_case(3..=usize::MAX, "unused" => panics "range end overflow"; "3 to usize::MAX inclusive")]
fn test_bytes_slice_range(range: impl RangeBounds<usize>, expected: &str) {
    assert_eq!(
        str::from_utf8(hello().slice(range).as_ref()).unwrap(),
        expected
    );
}

#[test_case(hello(), b"", ""; "any empty slice is empty")]
#[test_case(hello(), &hello_slice(..), "hello"; "full range is hello")]
#[test_case(hello(), &hello_slice(2..4), "ll"; "2 to 4 is ll")]
#[test_case(hello(), &hello_slice(0..=3), "hell"; "0 to 3 inclusive is hell")]
#[test_case(hello(), &Bytes::copy_from_slice(b"hello"), "unused" => panics "out of bounds"; "some other slice")]
#[test_case(hello_slice(1..), &hello_slice(..4), "unused" => panics "out of bounds"; "partial overlap start")]
#[test_case(hello_slice(..4), &hello_slice(1..), "unused" => panics "out of bounds"; "partial overlap end")]
fn test_bytes_slice_ref(bytes: Bytes, subset: &[u8], expected: &str) {
    assert_eq!(
        str::from_utf8(bytes.slice_ref(subset).expect("out of bounds").as_ref()).unwrap(),
        expected
    );
}

// Since we want a deterministic rng for the tests, we need to use a custom test runner instead of
// the !proptest macro.
fn test_runner() -> test_runner::TestRunner {
    test_runner::TestRunner::new_with_rng(
        test_runner::Config {
            failure_persistence: None,
            ..Default::default()
        },
        test_runner::TestRng::deterministic_rng(test_runner::RngAlgorithm::ChaCha),
    )
}

#[test]
fn test_bytes_clone_is_shallow() {
    test_runner()
        .run(&".*", |s| {
            let b1: Bytes = Bytes::from(s.clone());
            let b2: Bytes = b1.clone();
            // We know the bytes come from a String, so we can compare str values for pretty diffs
            assert_eq!(str::from_utf8(&b2), str::from_utf8(&b1));
            assert_eq!(b2, b1);
            // The pointers should be the same as well
            assert_eq!(b2.as_ptr(), b1.as_ptr());
            Ok(())
        })
        .unwrap();
}

#[test]
fn test_bytes_slice_is_shallow() {
    test_runner()
        .run(&(".*", 0..1000usize, 0..100usize), |(s, first, len)| {
            let start = usize::min(first, s.len());
            let len = usize::min(len, s.len() - start);
            let end = start + len;
            let b1: Bytes = Bytes::from(s.clone());

            let b2: Bytes = b1.slice(start..end);
            if b2.is_empty() {
                assert_eq!(b2, Bytes::empty());
            } else {
                // The slice should be a subset of the original
                assert!(b2.len() <= b1.len());
                assert!(b2.as_ptr() >= b1.as_ptr());
                assert!(b2.as_ptr() <= b1.as_ptr().wrapping_add(b1.len()));
            }
            Ok(())
        })
        .unwrap();
}
#[test]
fn test_bytes_slice_ref_is_shallow() {
    test_runner()
        .run(&(".*", 0..1000usize, 0..100usize), |(s, first, len)| {
            let start = usize::min(first, s.len());
            let len = usize::min(len, s.len() - start);
            let end = start + len;
            let b1: Bytes = Bytes::from(s.clone());
            let b2: Bytes = b1.slice(start..end);
            let b3: Bytes = b1.slice_ref(&b2).unwrap();
            if b2.is_empty() {
                assert_eq!(b3, Bytes::empty());
            } else {
                // The slice should be a subset of the original
                assert!(b3.len() <= b1.len());
                assert!(b3.as_ptr() >= b1.as_ptr());
                assert!(b3.as_ptr() <= b1.as_ptr().wrapping_add(b1.len()));
            }
            Ok(())
        })
        .unwrap();
}

#[test]
#[cfg_attr(miri, ignore)]
fn test_bytes_drop_frees_underlying() {
    let underlying = CountingU8::new(vec![1, 2, 3, 4, 5].into());
    let counter = underlying.counter();
    assert_eq!(get_counter(&counter), 0);
    let b1 = Bytes::from(underlying);
    assert_eq!(get_counter(&counter), 0);
    let b2 = b1.slice(2..);
    assert_eq!(get_counter(&counter), 0);
    drop(b1);
    assert_eq!(get_counter(&counter), 0);
    drop(b2);
    assert_eq!(get_counter(&counter), 1);
}

struct CountingU8 {
    inner: Box<[u8]>,
    count: Arc<AtomicUsize>,
}

impl CountingU8 {
    fn new(inner: Box<[u8]>) -> Self {
        Self {
            inner,
            count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn counter(&self) -> Arc<AtomicUsize> {
        self.count.clone()
    }
}

impl Drop for CountingU8 {
    fn drop(&mut self) {
        self.count.fetch_add(1, atomic::Ordering::Relaxed);
    }
}

impl AsRef<[u8]> for CountingU8 {
    fn as_ref(&self) -> &[u8] {
        self.inner.as_ref()
    }
}

impl UnderlyingBytes for CountingU8 {}

fn get_counter(counter: &Arc<AtomicUsize>) -> usize {
    counter.load(atomic::Ordering::Relaxed)
}
