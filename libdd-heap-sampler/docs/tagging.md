# Address tagging

When we sample an allocation we need a way to recognise, at free time, that
the pointer we're being handed back is one we sampled. That's what
`sample_flag.h` does. There are two completely different implementations,
one per architecture, because the best mechanism available differs a lot
between x86-64 and arm64.

## arm64: pointer tagging

arm64 supports Top-Byte Ignore (TBI). The CPU lets you set the top 8 bits 
of a 64-bit pointer to whatever you like and still dereference it normally, 
because those bits are ignored by the hardware. So instead of a header, 
we just set the top byte to a fixed marker (`0xDD`) when we sample an allocation.
This gives us the lowest possible cost on the `free` side, as we simply 
have to mask the pointer against our chosen flag bit.

The catch is that the kernel needs to be told, per thread, that it's OK for
tagged pointers to come back through syscalls without being rejected. That
is done through `prctl(PR_SET_TAGGED_ADDR_CTRL)` in
`dd_sample_flag_thread_init`, which must run once per thread before any
tagging happens on that thread. If the kernel refuses (older kernels,
seccomp policies, etc.) we disable sampling entirely on that thread. We might
want to revisit this in the future if we see that we consistently encounter
this case.

## x86-64: a header hidden before the user pointer

Although x86-64 typically supports pointer tagging and it was (briefly) enabled
in the kernel, it was pulled back out around Spectre due to security concerns. 

Instead we steal, bytes from the allocation itself - when we decide to sample, we ask the
underlying allocator for more memory than the caller requested, then place
the user-visible pointer some way into that block, leaving room for a
16-byte header just before it.

The header holds two 8-byte values:

* a magic constant (`DD_MAGIC`)
* the offset from the user pointer back to the raw pointer the allocator
  actually returned

At free time we look at the 16 bytes before the pointer we were given. If
the magic matches, we treat this as a sampled allocation, read the offset,
and use it to recover the raw pointer to hand to the real `free`. If the
magic doesn't match, it's an ordinary allocation and we pass it straight
through.

**Page Alignment** 
On the `free` side, if our pointer is within 16 bytes of the previous page
boundary we cannot safely read beneath it, as we may read into unmapped memory.

This means that when we sample an allocation, we must ensure that the resulting
pointer is _not_ within these 16 bytes. Preserving this property _and_ satisfying
the user's requested alignment is where the complexity in this mechanism comes from. 

We handle this by asking for enough extra space that we can always place the
user pointer at a safe offset. The base offset is `max(alignment, 16)` (room
for the header while staying aligned), and we reserve *twice* that in the
bumped allocation. The second copy is there so that if the first landing spot
would put us within 16 bytes of a page boundary, we can bump forward by
another `alignment` bytes and still fit inside the allocation.

**Zeroing the header on free**

Once we've confirmed a match and recovered the raw pointer, we zero out the
header. The underlying allocator can hand the same memory back out later for
an unsampled allocation, and if that allocation's contents happen to look
like our magic at the right offset, we'd wrongly treat it as sampled. Zeroing
prevents that.

**Keeping the three sites in sync**

The formula for the bumped allocation size has to be computed identically in
three places: when we decide how much extra to ask for (`bumped_alloc_size`
in `allocation_requested.c`), when we place the header and user pointer
(`x86_apply` in `sample_flag.h`), and when we work out how big the original
allocation was so we can free the right amount (`dd_allocation_freed_slow`
in `allocation_freed.c`). If any of these disagree we corrupt memory. Touch
one, check the other two.

**Alignment cap**

We don't sample allocations with alignment above 1024 bytes
(`DD_SAMPLE_ALIGNMENT_CAP`). Because the bumped size is
`user_size + 2 * max(alignment, 16)`, high alignments mean a lot of wasted
space just to hold a 16-byte header.

There is also a correctness reason for keeping the cap below a page. The x86-64
fast check refuses to read `user - 16` when `user` is in the first 16 bytes of a
page. That avoids touching an unmapped previous page for ordinary unsampled
pointers. A 4096-byte aligned pointer always has page offset 0, so a sampled
page-aligned allocation would not be recognised by free or realloc.

Some workloads/allocators may use big alignments to encourage the system to
hand back huge pages - see
[this blog post](https://mazzo.li/posts/check-huge-page.html) for context on
how common large-alignment allocations can be in practice. We need to monitor
how much we see this happening in real workloads, because if it's a lot we're
going to need to work out what to do with it.

## Why two check functions

You'll notice `dd_sample_flag_check` and `dd_sample_flag_peek` both exist.
`check` destroys the flag on the way out (clears the x86-64 header), which
is correct for a normal free: once we've recovered the raw pointer we never
need the flag on that address again. `peek` leaves the flag alone, which we
need for realloc; see `realloc.md` for why that distinction matters there.
