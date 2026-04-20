#include "allocation_freed.h"

size_t dd_allocation_freed(void *ptr, size_t size, size_t alignment) {
    (void)ptr;
    (void)alignment;
    return size;
}
