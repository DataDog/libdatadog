include ../heap.mk

.PHONY: build test lima-build lima-test clean

build:
	cargo build -p libdd-heap-sampler

test:
	cargo test -p libdd-heap-sampler

lima-build: lima-start-arm64
	limactl shell $(LIMA_INSTANCE_ARM64) sh -lc \
	  'cd "$(CURDIR)" && CARGO_TARGET_DIR="$(LIMA_TARGET_DIR)" cargo build -p libdd-heap-sampler'

lima-test: lima-start-arm64
	limactl shell $(LIMA_INSTANCE_ARM64) sh -lc \
	  'cd "$(CURDIR)" && CARGO_TARGET_DIR="$(LIMA_TARGET_DIR)" cargo test -p libdd-heap-sampler'

clean:
	cargo clean -p libdd-heap-sampler
