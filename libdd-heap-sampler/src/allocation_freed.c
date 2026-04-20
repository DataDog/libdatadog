#include <datadog/heap/allocation_freed.h>
#include <datadog/heap/probes.h>
#include <datadog/heap/sample_flag.h>

dd_alloc_freed_t dd_allocation_freed(void *ptr, size_t size, size_t alignment) {
    (void)alignment;

    dd_alloc_freed_t out = { .ptr = ptr, .size = size };

    void *raw;
    if (dd_sample_flag_check(ptr, &raw)) {
        dd_probe_free(ptr);
        out.ptr  = raw;
        out.size = size + dd_sample_flag_overhead();
    }

    return out;
}
