// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2024-Present Datadog, Inc.

use core::ops::Deref;
use std::marker::PhantomData;

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

    fn to_owned<T>(self) -> ToOwned<Self, T>
    where
        Self: Sized,
        for<'a> Self::Item<'a>: std::borrow::Borrow<T>,
    {
        ToOwned {
            iter: self,
            _marker: PhantomData,
        }
    }
}

/// Turn a collection of some sort into a [LendingIterator].
pub trait IntoLendingIterator {
    type Iter: LendingIterator;
    fn into_iter(self) -> Self::Iter;
}

pub struct ToOwned<I, T> {
    iter: I,
    _marker: PhantomData<T>,
}

impl<I, T> Iterator for ToOwned<I, T>
where
    I: LendingIterator,
    for<'a> I::Item<'a>: std::borrow::Borrow<T>,
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        match self.iter.next() {
            None => None,
            Some(item) => Some(std::borrow::ToOwned::to_owned(item)),
        }
    }

    fn count(self) -> usize {
        self.iter.count()
    }
}
