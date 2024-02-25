// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2024-Present Datadog, Inc.

/// The [LendingIterator] is a version of an [Iterator] that can yield items
/// with references into the lender. It is a well-known name and there are
/// multiple crates which offer it, with differences. The needs here are
/// small, and so rather than bring in a pre-1.0 crate, just make our own.
pub trait LendingIterator {
    type Item<'a>
    where
        Self: 'a;

    fn next(&mut self) -> Option<Self::Item<'_>>;

    fn count(self) -> usize;
}

/// Turn a collection of some sort into a [LendingIterator].
pub trait IntoLendingIterator {
    type Iter: LendingIterator;
    fn into_iter(self) -> Self::Iter;
}
