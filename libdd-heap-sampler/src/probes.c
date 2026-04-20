#include <datadog/heap/probes.h>

void dd_probe_alloc(void *user, uint64_t size, uint64_t weight) {
    DTRACE_PROBE3(ddheap, alloc, user, size, weight);
}

void dd_probe_free(void *ptr) {
    DTRACE_PROBE1(ddheap, free, ptr);
}
