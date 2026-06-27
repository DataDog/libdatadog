# libdd-collections

Panic-free collection types for constrained and FFI-sensitive code paths.

This crate currently provides a `Vec` implementation with an entirely safe API
surface. It exposes fallible allocation operations and deliberately avoids
indexing traits that would add panic paths for out-of-bounds access.
