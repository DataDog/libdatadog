// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod sealed {
    pub trait Sealed {}

    impl<T> Sealed for Vec<T> {}
}

/// A trait for extending the capabilities of the built-in Vec.
pub trait VecExt<T>: sealed::Sealed {
    /// Appends an element if there is sufficient spare capacity, or if the
    /// vec successfully reserves more capacity; otherwise an error is
    /// returned with the element.
    ///
    /// This is similar to calling [`Vec::try_reserve`] and then [`Vec::push`].
    /// However, it's not _just_ a convenience: it avoids a call to `Vec`'s
    /// internal `grow_one` method which `Vec::push` has, which means that the
    /// `try_push` function has no paths which can panic on release builds.
    fn try_push(&mut self, value: T) -> Result<(), T>;
}

impl<T> VecExt<T> for Vec<T> {
    fn try_push(&mut self, value: T) -> Result<(), T> {
        let len = self.len();
        if self.try_reserve(1).is_err() {
            return Err(value);
        }
        // SAFETY: try_reserve ensures there is at least one item of capacity.
        let end = unsafe { self.as_mut_ptr().add(len) };
        // SAFETY: Vec ensures proper alignment, and since there is unused
        // capacity, the address is valid for writes.
        unsafe { std::ptr::write(end, value) };
        // SAFETY: the len is less than or equal to the capacity due to the
        // try_reserve, and we initialized the new slot with the ptr::write.
        unsafe { self.set_len(len + 1) };
        Ok(())
    }
}
