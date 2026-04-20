### jemalloc_hook and mimalloc_hook and maybe even tcmalloc_hook
Allocators often have a sampling hook mechanism of their own - with the notable exceptions of glibc and musl. For each of these
we will provide an implementation of that hook on top of the samplers library.
