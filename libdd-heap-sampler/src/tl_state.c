#include "tl_state.h"

#include <pthread.h>
#include <stdlib.h>
#include <sys/random.h>

static pthread_once_t s_key_once = PTHREAD_ONCE_INIT;
static pthread_key_t  s_key;

static void tl_state_destroy(void *s) {
    free(s);
}

static void tl_state_make_key(void) {
    pthread_key_create(&s_key, tl_state_destroy);
}

dd_tl_state_t *dd_tl_state_get(void) {
    pthread_once(&s_key_once, tl_state_make_key);
    return (dd_tl_state_t *)pthread_getspecific(s_key);
}

/**
 Sets up the TL state for this thread the first time we are called.
 **/ 
dd_tl_state_t *dd_tl_state_init(void) {
    pthread_once(&s_key_once, tl_state_make_key);
    if (pthread_getspecific(s_key)) return NULL;

    dd_tl_state_t *st = (dd_tl_state_t *)calloc(1, sizeof(*st));
    if (!st) return NULL;

    if (getentropy(&st->rng, sizeof(st->rng)) != 0 || !st->rng) {
        st->rng = (uint32_t)((uintptr_t)st ^ (uintptr_t)pthread_self()) | 1u;
    }

    // We copy this in so that we can _potentially_ choose to adjust it dynamically
    // during runtime. Potentially. One day.
    st->sampling_interval = DD_SAMPLING_INTERVAL_DEFAULT;

    if (pthread_setspecific(s_key, st) != 0) {
        free(st);
        return NULL;
    }
    return st;
}
