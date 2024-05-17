# Sidecar Mock Generator

The mock generator analyzes a shared object file or executable (henceforth called _the binary_) and a set of passed object files.

It will compute the set of symbols existing in the binary, but not in the passed object files.

For each symbol in that set it emits a dummy definition, with correct sizes, in a C file.

That C file then can be compiled by the caller of the sidecar_mockgen to load along the sidecar.

## Problem solved

Under Unix systems, the dynamic linker expects all non-weak symbols declared in a shared library to be present.

However, the sidecar is not a separate executable, but loads itself via a trampoline. On top of that the sidecar may be linked against other code, which has other external dependencies.

These external dependencies are not present though when the sidecar is loaded via the trampoline. Thus this sidecar_mockgen is needed to insert these symbols with dummy definitions when the sidecar is loaded.
