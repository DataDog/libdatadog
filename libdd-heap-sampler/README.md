> [!WARNING]
> This is just a rough sketch of the outside of this API to push the heap profiling design along. There is a lot missing!

# Heap Profiling - Datadog Shared Lib

This library forms the foundation of Datadog's application-side heap profiling support; you can read more about
this initiative [over here](https://docs.google.com/document/d/1cy6OUisjW4_vAIaAu9G5l3RGuPPLT_MRZuhDPJHxXuw/edit?tab=t.z91c4h77tkuy#heading=h.dxy2mnwfeqrl). 

It provides sampling functions that can be used to wrap each of the primary allocation and free functions of an arbitrary allocator.

For allocations that are sampled as well as the corresponding frees of these allocations, appropriate USDTs are
emitted such that an external process such as the [eBPF full host profiler](TODO) can collect the samples as
well as the stack trace at the time they are emitted to ultimately emit as a heap profiling event stream.

The library is made up of multiple components; during the PoC phase, we should expect some level of "throw it all at the wall
and see what sticks", and would realistically expect the set of pieces reduces over time!


**TL;DR - what can I do _today_?**

If you want to **see it working right now**, the fastest path is the Rust allocator demo in [`libdd-heap-allocator`](../libdd-heap-allocator#running-the-demo), which wraps the system allocator, fires USDT probes on every heap event, and lets you observe them live with `bpftrace`.


## Components

### samplers
These are the foundational functions themselves containing the sampling logic and USDTs, and are intended to be used
within higher order constructs that bind them back to concrete allocator callsites. 
They are responsible for deciding whether or not to sample, and storing the information required to decide later on, at `free` time, if the given allocation _was_ sampled. We will cover:

**USDTs**

The actual USDTs emitted are:

* `ddheap:alloc(void *user, uint64_t size, uint64_t weight)` — fired on sampled allocations; `user` is the user-visible pointer, `size` in bytes, `weight` is the unbiased size estimator (`nsamples * interval`)
* `ddheap:free(void *ptr)` — fired when a previously-sampled allocation is freed
* `ddheap:mmap` - TODO 
* `ddheap:munmap` - TODO

**Allocations**

By splitting into `requested` and `created`, these are designed to be generic across different allocation functions (e.g. `malloc`, `operator new`, `aligned_alloc`, etc.). The job of binding these back to concrete callsites in a process is left to the other components - e.g. `libddd-heap-gotter`, `libdd-heap-allocator`, etc.     

The allocation-side pair is declared `static inline __attribute__((always_inline))` so the non-sampled fast path inlines into the wrapper with no function-call overhead.

Note that the functions on the _allocation_ side will return an _updated_ allocation size. This will generally be the same as the requested allocation size, but may not always be as the sampling mechanism may choose to increase the allocation size in order to ease the process of tracking sampling decisions. The caller should pass this returned value through verbatim to the allocator it is wrapping.
