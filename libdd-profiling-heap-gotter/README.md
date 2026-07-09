# libdd-heap-gotter

GOTter implements our GOT-patching mechanism to wrap (dynamically!) linked allocators in a running process.
This follows the same approach as `ddprof`, and may prove useful to inject via our tracing libraries into running processes such as python.

It contains:

* A set of functions such as `gotter_malloc` that will be used to override the _originals_ of these functions
* Overrides for _other bits_ we need for this to work robustly in a running process (`dlopen` to re-scan on new library load, `pthread_create` to materialise sampler TLS on new threads)
* A function to install the overrides in a running process `install_heap_overrides()`

Not yet covered: `operator new` / `operator delete`, `mmap`/`munmap`, jemalloc-specific `*allocx` variants, and `pthread_atfork` child-handler for clean `fork()` state reset.

This will be used in places such as the python profiler to install the heap profiler at runtime to capture native allocations.
