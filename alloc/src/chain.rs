// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::LinearAllocator;
use crate::{AllocError, Allocator};
use core::alloc::Layout;
use core::cell::UnsafeCell;
use core::mem::size_of;
use core::ptr::NonNull;

/// [ChainAllocator] is an arena allocator, meaning that deallocating
/// individual allocations made by this allocator does nothing. Instead, the
/// whole backing memory is dropped at once.  Destructors for these objects
/// are not called automatically and must be done by the caller if it's
/// necessary.
///
/// [ChainAllocator] creates a new [LinearAllocator] when the current one
/// doesn't have enough space for the requested allocation, and then links the
/// new [LinearAllocator] to the previous one, creating a chain. This is where
/// its name comes from.
pub struct ChainAllocator<A: Allocator + Clone> {
    top: UnsafeCell<ChainNodePtr<A>>,
    /// The size hint for the linear allocator's chunk.
    node_size: usize,
    allocator: A,
}

#[derive(Clone, Copy)]
struct ChainNodePtr<A: Allocator> {
    ptr: Option<NonNull<ChainNode<A>>>,
}

impl<A: Allocator> ChainNodePtr<A> {
    #[inline]
    fn as_mut_ptr(&self) -> *mut ChainNode<A> {
        match self.ptr {
            Some(non_null) => non_null.as_ptr(),
            None => core::ptr::null_mut(),
        }
    }
}

impl<A: Allocator> ChainNodePtr<A> {
    const fn new() -> Self {
        Self { ptr: None }
    }

    fn as_ref(&self) -> Option<&ChainNode<A>> {
        // SAFETY: active as long as not-null, never give out mut refs.
        self.ptr.map(|p| unsafe { p.as_ref() })
    }
}

/// The node exists inside the allocation owned by `linear`.
struct ChainNode<A: Allocator> {
    prev: UnsafeCell<ChainNodePtr<A>>,
    linear: LinearAllocator<A>,
}

impl<A: Allocator> ChainNode<A> {
    #[inline]
    fn prev_ptr(&self) -> *mut ChainNode<A> {
        // SAFETY: all references are temporary and do not escape local scope,
        // preventing multiple references.
        unsafe { (*self.prev.get()).as_mut_ptr() }
    }
}

unsafe impl<A: Allocator + Clone> Send for ChainAllocator<A> {}

impl<A: Allocator> ChainNode<A> {
    fn remaining_capacity(&self) -> usize {
        self.linear.remaining_capacity()
    }
}

impl<A: Allocator + Clone> ChainAllocator<A> {
    /// The amount of bytes used by the [ChainAllocator] at the start of each
    /// chunk of the chain for bookkeeping.
    pub const CHAIN_NODE_OVERHEAD: usize = size_of::<ChainNode<A>>();

    /// The individual nodes need to be big enough that the overhead of a chain
    /// is worth it. This is somewhat arbitrarily chosen at the moment.
    const MIN_NODE_SIZE: usize = 4 * Self::CHAIN_NODE_OVERHEAD;

    /// Creates a new [ChainAllocator]. The `chunk_size_hint` is used as a
    /// size hint when creating new chunks of the chain. Note that the
    /// [ChainAllocator] will use some bytes at the beginning of each chunk of
    /// the chain. The number of bytes is [Self::CHAIN_NODE_OVERHEAD]. Keep
    /// this in mind when sizing your hint if you are trying to be precise,
    /// such as making sure a specific object fits.
    pub const fn new_in(chunk_size_hint: usize, allocator: A) -> Self {
        Self {
            top: UnsafeCell::new(ChainNodePtr::new()),
            // max is not a const fn, do it manually.
            node_size: if chunk_size_hint < Self::MIN_NODE_SIZE {
                Self::MIN_NODE_SIZE
            } else {
                chunk_size_hint
            },
            allocator,
        }
    }

    #[cold]
    #[inline(never)]
    fn grow(&self) -> Result<(), AllocError> {
        let top = self.top.get();
        let chain_layout = Layout::new::<ChainNode<A>>();

        let linear = {
            let layout = Layout::from_size_align(self.node_size, chain_layout.align())
                .map_err(|_| AllocError)?;
            LinearAllocator::new_in(layout, self.allocator.clone())?
        };

        // This shouldn't fail.
        let chain_node_addr = linear
            .allocate(chain_layout)?
            .as_ptr()
            .cast::<ChainNode<A>>();
        let chain_node = {
            // SAFETY: If non-null, this is a valid pointer, and the reference
            // is temporary, as all references for the chain nodes are.
            let ptr = unsafe { (*top).ptr };
            ChainNode {
                prev: UnsafeCell::new(ChainNodePtr { ptr }),
                linear,
            }
        };

        // SAFETY: this is a write operation to freshly allocated memory which
        // has the correct layout.
        unsafe { chain_node_addr.write(chain_node) };

        let chain_node_ptr = ChainNodePtr {
            // SAFETY: derived from allocation (not null).
            ptr: Some(unsafe { NonNull::new_unchecked(chain_node_addr) }),
        };

        // SAFETY: the value is just a pointer, no drops need to occur.
        // Additionally, references are always temporary for the top, so this
        // write will not violate aliasing rules.
        unsafe { self.top.get().write(chain_node_ptr) };

        Ok(())
    }

    fn capacity_helper(mut ptr: *mut ChainNode<A>) -> usize {
        let mut capacity = 0_usize;
        // SAFETY: if non-null, it's a valid pointer. The reference is
        // short-lived as usual to avoid aliasing issues.
        while let Some(chain_node) = unsafe { ptr.as_ref() } {
            capacity += chain_node.linear.reserved_bytes();
            ptr = chain_node.prev_ptr();
        }
        capacity
    }

    fn top_chain_node_ptr(&self) -> *mut ChainNode<A> {
        // SAFETY: This is never exposed to users, and never used internally
        // in a way it will provide simultaneous mutable references.
        unsafe { (*self.top.get()).as_mut_ptr() }
    }

    /// Get the number of bytes allocated, including bytes for overhead.
    /// It does not count space it _could_ allocate still, such as unused space
    /// at the end of the top node in the chain. It does count unallocated
    /// space at the end of previous nodes in the chain.
    pub fn used_bytes(&self) -> usize {
        let mut chain_node_ptr = self.top_chain_node_ptr();
        let Some(chain_node) = (unsafe { chain_node_ptr.as_ref() }) else {
            return 0;
        };

        // The top node is the one that new allocations are made from, so it
        // is likely only partially full.
        let size = {
            let size = chain_node.linear.used_bytes();
            chain_node_ptr = chain_node.prev_ptr();
            size
        };

        // However, the previous nodes in the chain are all full, or at least
        // they should be considered full as any unused space at the end of
        // the allocation won't get used. So fetch `capacity` for previous
        // nodes in the chain.
        let prev_capacity = Self::capacity_helper(chain_node_ptr);
        size + prev_capacity
    }

    /// Get the number of bytes allocated by the underlying allocators for
    /// this chain. This number is greater than or equal to [Self::used_bytes].
    pub fn reserved_bytes(&self) -> usize {
        let ptr = self.top_chain_node_ptr();
        Self::capacity_helper(ptr)
    }

    /// Gets the number of bytes that can be allocated without requesting more
    /// from the underlying allocator.
    pub fn remaining_capacity(&self) -> usize {
        // Only need to look at the top node of the chain, all the previous
        // nodes are considered full.
        let chain_ptr = self.top.get();
        // SAFETY: If non-null, this is a valid pointer, and the reference is
        // temporary, as all references for the chain nodes are.
        let top = unsafe { (*chain_ptr).as_ref() };
        top.map(ChainNode::remaining_capacity).unwrap_or(0)
    }
}

unsafe impl<A: Allocator + Clone> Allocator for ChainAllocator<A> {
    #[cfg_attr(debug_assertions, track_caller)]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let layout = layout.pad_to_align();

        // Too large for ChainAllocator to deal with.
        let header_overhead = size_of::<ChainNode<A>>();
        let maximum_capacity = self.node_size - header_overhead;
        if layout.size() > maximum_capacity {
            return Err(AllocError);
        }

        let remaining_capacity = self.remaining_capacity();
        if layout.size() > remaining_capacity {
            self.grow()?;
        }

        // At this point:
        //  1. There's a top node.
        //  2. It has enough capacity for the allocation.

        let top = self.top.get();
        let chain_node = unsafe { (*top).as_ref().unwrap_unchecked() };

        debug_assert!(chain_node.remaining_capacity() >= layout.size());

        let result = chain_node.linear.allocate(layout);
        // If this fails, there's a bug in the allocator.
        debug_assert!(result.is_ok());
        result
    }

    unsafe fn deallocate(&self, _ptr: NonNull<u8>, _layout: Layout) {
        // This is an arena. It does batch de-allocation when dropped.
    }
}

impl<A: Allocator + Clone> Drop for ChainAllocator<A> {
    fn drop(&mut self) {
        // SAFETY: top node is alive, type is fine to `read` because it is
        // behind a cell type, so it will not get double-dropped.
        let mut chain_node_ptr = unsafe { self.top.get().read() };

        loop {
            match chain_node_ptr.ptr {
                None => break,
                Some(non_null) => {
                    // SAFETY: the chunk hasn't been dropped yet, so the ptr
                    // to the chunk is alive. The prev pointer of the chunk is
                    // moved to the stack before the chunk is dropped, so it's
                    // alive and valid after the chunk is dropped below.
                    chain_node_ptr = unsafe {
                        core::ptr::addr_of!((*non_null.as_ptr()).prev)
                            .read()
                            .get()
                            .read()
                    };

                    // SAFETY: the chunk hasn't been dropped yet, and the
                    // linear allocator lives in the chunk. Moving it to the
                    // stack before dropping it avoids a fringe lifetime issue
                    // which could happen occur with drop_in_place instead.
                    let alloc =
                        unsafe { core::ptr::addr_of_mut!((*non_null.as_ptr()).linear).read() };

                    // The drop will happen anyway, but being explicit.
                    drop(alloc);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use allocator_api2::alloc::Global;

    #[test]
    fn test_basics() {
        let allocator = ChainAllocator::new_in(4096, Global);
        let layout = Layout::new::<[u8; 8]>();
        let ptr = allocator.allocate(layout).unwrap();

        // deallocate doesn't return memory to the allocator, but it shouldn't
        // panic, as that prevents its use in containers like Vec.
        unsafe { allocator.deallocate(ptr.cast(), layout) };
    }

    #[track_caller]
    fn fill_to_capacity<A: Allocator + Clone>(allocator: &ChainAllocator<A>) {
        let remaining_capacity = allocator.remaining_capacity();
        if remaining_capacity != 0 {
            let layout = Layout::array::<u8>(remaining_capacity).unwrap();
            let ptr = allocator.allocate(layout).unwrap();
            // Doesn't return memory, just ensuring we don't panic.
            unsafe { allocator.deallocate(ptr.cast(), layout) };
        }
        let remaining_capacity = allocator.remaining_capacity();
        assert_eq!(0, remaining_capacity);
    }

    #[test]
    fn test_growth() {
        let page_size = crate::os::page_size().unwrap();
        let allocator = ChainAllocator::new_in(page_size, Global);

        let bool_layout = Layout::new::<bool>();

        // test that it fills to capacity a few times.
        for _ in 0..100 {
            fill_to_capacity(&allocator);

            // This check is theoretically redundant because fill_to_capacity
            // ensures this already, but this tests using the public API.
            let size = allocator.used_bytes();
            let capacity = allocator.reserved_bytes();
            assert_eq!(size, capacity);

            // Trigger it to grow.
            let ptr = allocator.allocate(bool_layout).unwrap();

            // Doesn't free, shouldn't panic though.
            unsafe { allocator.deallocate(ptr.cast(), bool_layout) };

            // The growth means there should be many used bytes.
            let size = allocator.used_bytes();
            let capacity = allocator.reserved_bytes();
            assert!(size < capacity, "failed: {size} < {capacity}");
        }

        let reserved_bytes = allocator.reserved_bytes();
        // The allocations can theoretically be over-allocated, so use >= to
        // do the comparison.
        assert!(reserved_bytes >= page_size * 100);

        // Everything is filled to capacity except the last iteration.
        let used_bytes = allocator.used_bytes();
        assert!(
            used_bytes < reserved_bytes,
            "failed: {used_bytes} < {reserved_bytes}"
        );
    }
}
