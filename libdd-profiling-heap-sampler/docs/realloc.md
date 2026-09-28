# Realloc handling

When we get a realloc, we treat it as a separate free and a new allocation
to keep things simple. This means a resized allocation that was sampled
before the resize is not sampled after it: we report the free and just let
the new block go untracked, rather than trying to carry the sample
forward. Doing that properly would mean stamping a fresh header with the
original user-requested size, which we don't currently store, so it's a
fair chunk of extra work for a case that's rare enough not to skew results
much. Worth revisiting later.

## What we need to preserve

Whatever we do here has to keep a couple of properties intact:

* If the real `realloc` fails, the old pointer must still be exactly as
  usable as it was before we touched anything, including still being
  recognised as sampled if it was sampled going in.
* We must never hand back a pointer with a stale or wrong header
* The common, unsampled path should stay as close as possible to a plain
  passthrough to the real `realloc`
  
## The algorithm

The sampler sorts every call into one of four cases in
`dd_allocation_realloc_prepare(old_user, new_size)`:

* `ptr == NULL` is just `malloc(size)`, so it runs through the normal
  allocation sampling path. `prepare` returns the raw size to pass to the
  real allocator, and `commit` pairs the result with
  `dd_allocation_created`.
* `size == 0` is just `free(ptr)` for the allocators we hook, so
  `prepare` consumes the sampler flag if there is one and returns the raw
  pointer to forward to the real allocator.
* An unsampled `ptr` is a plain passthrough to the real `realloc`. We
  don't start sampling the resulting block here either, for the same
  reason as above: we didn't want realloc behaviour to depend on which
  side of the sampling coin flip the original allocation happened to land
  on.
* A sampled `ptr` is where the interesting work happens. `prepare` uses
  the non-destructive `peek` (see [tagging.md](tagging.md)) to find the
  real allocation start (`old_raw`) and the offset of the user pointer
  (`old_offset`). It asks the frontend to call the real allocator with
  `old_raw` and `new_size + old_offset`.

That extra `old_offset` bytes exist because libc's realloc will copy the
old data forward from the old raw pointer, so the old header ends up
occupying the front of the new block too unless we make room for it.
Importantly, the sampled-old prepare path leaves the old allocation's flag
alone, so if the real realloc fails, the pointer is untouched and a later
`free` on it still behaves correctly.

The frontend calls the real `realloc` with the pointer and size `prepare`
computed, and passes whatever comes back into
`dd_allocation_realloc_commit(old_user, new_raw, prep)`.

For the sampled-old case:

* If `new_raw` is `NULL`, realloc failed. We return `NULL` and leave
  everything else alone.
* Otherwise, the old user bytes copied by libc are sitting at
  `new_raw + old_offset` instead of at the start of the block, so we
  `memmove` them down to the front. It has to be `memmove`, not `memcpy`,
  because when realloc grows a block in place, `new_raw` and `old_raw` are
  the same address and the ranges overlap.
* We fire the `ddheap:free` USDT for the old address, since as far as the
  profiler is concerned that allocation no longer exists.
* We return `new_raw` as a plain, unsampled pointer.
