include ../heap.mk

.PHONY: build test lima-demo-arm64 lima-demo-amd64 clean

build:
	cargo build -p libdd-heap-allocator

test:
	cargo test -p libdd-heap-allocator

lima-demo-arm64: lima-start-arm64
	limactl shell $(LIMA_INSTANCE_ARM64) sh -lc \
	  'WORKSPACE_ROOT="$(LIMA_WORKSPACE_ROOT)" CARGO_TARGET_DIR="$(LIMA_TARGET_DIR)" \
	   bash "$(LIMA_WORKSPACE_ROOT)/libdd-heap-allocator/scripts/run-usdt-demo.sh"'

lima-demo-amd64: lima-start-amd64
	limactl shell $(LIMA_INSTANCE_AMD64) sh -lc \
	  'WORKSPACE_ROOT="$(LIMA_WORKSPACE_ROOT)" CARGO_TARGET_DIR="$(LIMA_TARGET_DIR)" \
	   bash "$(LIMA_WORKSPACE_ROOT)/libdd-heap-allocator/scripts/run-usdt-demo.sh"'

clean:
	cargo clean -p libdd-heap-allocator
