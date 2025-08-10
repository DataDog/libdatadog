use datadog_alloc::{AllocError, Allocator, ChainAllocator};
use std::alloc::{Layout, LayoutError};
use std::borrow::Borrow;
use std::hash;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::ops::Deref;
use std::ptr::NonNull;

/// A struct which acts like a thin &str. It does this by storing the size
/// of the string just before the bytes of the string.
#[derive(Copy, Clone)]
#[repr(C)]
pub struct ThinStr<'a> {
    thin_ptr: ThinPtr,

    /// Since [`ThinStr`] doesn't hold a reference but acts like one, indicate
    /// this to the compiler with phantom data. This takes up no space.
    _marker: PhantomData<&'a str>,
}

unsafe impl Sync for ThinStr<'static> {}

pub trait ArenaAllocator: Allocator {}

impl<A: Allocator + Clone> ArenaAllocator for ChainAllocator<A> {}

impl ThinStr<'static> {
    pub const fn new() -> ThinStr<'static> {
        Self {
            thin_ptr: EMPTY_INLINE_STRING.as_thin_ptr(),
            _marker: PhantomData,
        }
    }

    pub const fn end_timestamp_ns() -> ThinStr<'static> {
        Self {
            thin_ptr: END_TIMESTAMP_NS.as_thin_ptr(),
            _marker: PhantomData,
        }
    }

    pub const fn local_root_span_id() -> ThinStr<'static> {
        Self {
            thin_ptr: LOCAL_ROOT_SPAN_ID.as_thin_ptr(),
            _marker: PhantomData,
        }
    }

    pub const fn trace_endpoint() -> ThinStr<'static> {
        Self {
            thin_ptr: TRACE_ENDPOINT.as_thin_ptr(),
            _marker: PhantomData,
        }
    }

    pub const fn span_id() -> ThinStr<'static> {
        Self {
            thin_ptr: SPAN_ID.as_thin_ptr(),
            _marker: PhantomData,
        }
    }
}

impl Default for ThinStr<'static> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> ThinStr<'a> {
    pub fn layout_for(str: &str) -> Result<Layout, LayoutError> {
        let header = Layout::new::<[u8; USIZE_WIDTH]>();
        let data = Layout::for_value(str);
        let (layout, _) = header.extend(data)?;
        Ok(layout)
    }

    pub fn try_allocate_for<A: Allocator>(
        str: &str,
        allocator: &A,
    ) -> Result<NonNull<[MaybeUninit<u8>]>, AllocError> {
        let Ok(layout) = Self::layout_for(str) else {
            return Err(AllocError);
        };

        let obj = allocator.allocate(layout)?;
        let ptr = obj.cast::<MaybeUninit<u8>>();
        Ok(NonNull::slice_from_raw_parts(ptr, obj.len()))
    }

    pub unsafe fn from_str_in_unchecked(
        str: &str,
        spare_capacity: &'a mut [MaybeUninit<u8>],
    ) -> Self {
        let allocation = spare_capacity.as_mut_ptr().cast::<u8>();

        let size = allocation.cast::<[u8; USIZE_WIDTH]>();
        // SAFETY: writing into uninitialized new allocation at correct place.
        unsafe { size.write(str.len().to_ne_bytes()) };

        // SAFETY: the data pointer is just after the header, and the
        // allocation is at least that long.
        let data = unsafe { allocation.add(USIZE_WIDTH) };

        // SAFETY: the allocation is big enough, locations are distinct, and
        // the alignment is 1 (so it's always aligned), and the memory is safe
        // for writing.
        unsafe { core::ptr::copy_nonoverlapping(str.as_bytes().as_ptr(), data, str.len()) };

        let size_ptr = unsafe { NonNull::new_unchecked(allocation) };
        let thin_ptr = ThinPtr { size_ptr };
        let _marker = PhantomData;
        Self { thin_ptr, _marker }
    }

    /// Tries to create a [`ThinStr`] in the uninitialized space.
    ///
    /// # Errors
    ///
    /// Returns an error if the string plus its header are too big to fit in
    /// the spare capacity.
    pub fn try_from_str_in(
        str: &str,
        spare_capacity: &'a mut [MaybeUninit<u8>],
    ) -> Result<Self, AllocError> {
        // Note that overflow checks can be omitted here because:
        //  1. The largest possible string has a len of `isize::MAX`.
        //  2. A `usize` is around 4 or 8 bytes.
        //  3. The sum of 1 and 2 cannot overflow `usize`.
        // It might overflow an isize, but we don't have to check that either,
        // because `spare_capacity` cannot have a length longer than
        // `isize::MAX`, so if it fits in the spare capacity, it must be
        // small enough to fit in an `isize` too.
        let inline_size = str.len() + USIZE_WIDTH;
        if inline_size > spare_capacity.len() {
            return Err(AllocError);
        }

        // SAFETY: we just checked that string's bytes and header can fit.
        Ok(unsafe { Self::from_str_in_unchecked(str, spare_capacity) })
    }

    /// Gets the layout of a ThinStr, such as to deallocate it.
    #[allow(unused)]
    #[inline]
    pub fn layout(&self) -> Layout {
        self.thin_ptr.layout()
    }
}

impl Deref for ThinStr<'_> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        let slice = {
            let ptr_slice = self.thin_ptr.wide_data_ptr();

            // SAFETY: bytes are never handed out as mut, so const slices are
            // not going to break aliasing rules, and this is the correct
            // lifetime for the data.
            unsafe { &*ptr_slice.as_ptr() }
        };

        // SAFETY: since this is a copy of a valid utf-8 string, then it must
        // also be valid utf-8.
        unsafe { core::str::from_utf8_unchecked(slice) }
    }
}

impl hash::Hash for ThinStr<'_> {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.deref().hash(state)
    }
}

impl PartialEq for ThinStr<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.deref().eq(other.deref())
    }
}

impl Eq for ThinStr<'_> {}

impl Borrow<str> for ThinStr<'_> {
    fn borrow(&self) -> &str {
        self.deref()
    }
}

#[repr(transparent)]
#[derive(Clone, Copy)]
struct ThinPtr {
    /// Points to the beginning of a struct which looks like this:
    /// ```
    /// #[repr(C)]
    /// struct InlineString {
    ///     /// Stores the len of `data`.
    ///     size: [u8; core::mem::size_of::<usize>()],
    ///     data: [u8],
    /// }
    /// ```
    size_ptr: NonNull<u8>,
}

impl ThinPtr {
    /// Reads the size prefix to get the length of the string.
    const fn len(self) -> usize {
        // SAFETY: ThinStr points to the size prefix of the string.
        let size = unsafe { self.size_ptr.cast::<[u8; USIZE_WIDTH]>().as_ptr().read() };
        usize::from_ne_bytes(size)
    }

    /// Returns a pointer to the string data (not to the header).
    const fn data_ptr(self) -> NonNull<u8> {
        // SAFETY: ThinStr points to the size prefix of the string, and the
        // string data is located immediately after without padding.
        unsafe { self.size_ptr.add(USIZE_WIDTH) }
    }

    /// Returns a pointer slice to the string data.
    const fn wide_data_ptr(self) -> NonNull<[u8]> {
        let len = self.len();
        NonNull::slice_from_raw_parts(self.data_ptr(), len)
    }

    /// Gets the layout of a ThinStr, such as to deallocate it.
    #[allow(unused)]
    #[inline]
    fn layout(self) -> Layout {
        let len = self.len();
        // SAFETY: since this object exists, its layout must be valid.
        unsafe { Layout::from_size_align_unchecked(len + USIZE_WIDTH, 1) }
    }
}

#[repr(C)]
struct StaticInlineString<const N: usize> {
    /// Stores the len of `data`.
    size: [u8; core::mem::size_of::<usize>()],
    data: [u8; N],
}

impl<const N: usize> StaticInlineString<N> {
    const fn as_thin_ptr(&self) -> ThinPtr {
        let ptr = core::ptr::addr_of!(EMPTY_INLINE_STRING).cast::<u8>();
        // SAFETY: derived from static address, and ThinStr does not allow
        // modifications, so the mut-cast is also fine.
        let size_ptr = unsafe { NonNull::new_unchecked(ptr.cast_mut()) };
        ThinPtr { size_ptr }
    }
}

const USIZE_WIDTH: usize = core::mem::size_of::<usize>();

const fn inline_string<const N: usize>(str: &str) -> StaticInlineString<N> {
    if str.len() != N {
        panic!("string length and storage mismatch for StaticInlineString")
    }
    StaticInlineString::<N> {
        size: N.to_ne_bytes(),
        data: {
            let src = str.as_bytes();
            let mut dst = [0u8; N];
            let mut i = 0usize;
            while i < N {
                dst[i] = src[i];
                i += 1;
            }
            dst
        }
    }
}

static EMPTY_INLINE_STRING: StaticInlineString<0> = inline_string("");
static END_TIMESTAMP_NS: StaticInlineString<16> = inline_string("end_timestamp_ns");
static LOCAL_ROOT_SPAN_ID: StaticInlineString<18> = inline_string("local root span id");
static TRACE_ENDPOINT: StaticInlineString<14> = inline_string("trace endpoint");
static SPAN_ID: StaticInlineString<7> = inline_string("span id");


#[no_mangle]
pub static DDOG_PROF_WELL_KNOWN_STRINGS: [ThinStr; 5] = [
    ThinStr::new(),
    ThinStr::end_timestamp_ns(),
    ThinStr::local_root_span_id(),
    ThinStr::trace_endpoint(),
    ThinStr::span_id(),
];

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_alloc::Global;

    const TEST_STRINGS: [&str; 5] = [
        "datadog",
        "MyNamespace.MyClass.MyMethod(Int32 id, String name)",
        "/var/run/datadog/apm.socket",
        "[truncated]",
        "Sidekiq::❨╯°□°❩╯︵┻━┻",
    ];

    #[test]
    fn test_allocation_and_deallocation() {
        let alloc = &Global;

        let mut thin_strs: Vec<ThinStr> = TEST_STRINGS
            .iter()
            .copied()
            .map(|str| {
                let obj = ThinStr::try_allocate_for(str, alloc).unwrap();
                // SAFETY: just allocated the bytes, no other references exist,
                // so we can safely turn it into `&mut [MaybeUninit<u8>]`.
                let uninit = unsafe { &mut *obj.as_ptr() };
                let thin_str = ThinStr::try_from_str_in(str, uninit).unwrap();
                let actual = thin_str.deref();
                assert_eq!(str, actual);
                thin_str
            })
            .collect();

        // This could detect out-of-bounds reads.
        for (thin_str, str) in thin_strs.iter().zip(TEST_STRINGS) {
            let actual = thin_str.deref();
            assert_eq!(str, actual);
        }

        for thin_str in thin_strs.drain(..) {
            unsafe { alloc.deallocate(thin_str.thin_ptr.size_ptr, thin_str.layout()) };
        }
    }
}
