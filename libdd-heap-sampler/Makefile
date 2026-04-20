# Convenience wrappers for exercising libdd-heap-sampler on Linux from a
# macOS host. Everything routes through cargo so the build stays in lockstep
# with the rest of the workspace.

LIMA_INSTANCE := libdd-heap-sampler
LIMA_TEMPLATE := lima-arm64.yaml

# Keep the Linux target directory off the macOS shared mount so native and
# Lima builds don't corrupt each other's artifact cache. $$HOME expands
# inside the VM to the Lima user's home (/home/<user>.linux), which is not
# mounted from macOS.
LIMA_TARGET_DIR := $$HOME/libdatadog-target

.PHONY: build test lima-build lima-test lima-shell lima-start lima-destroy clean

build:
	cargo build -p libdd-heap-sampler

test:
	cargo test -p libdd-heap-sampler

lima-build: lima-start
	limactl shell $(LIMA_INSTANCE) sh -lc 'cd "$(CURDIR)" && CARGO_TARGET_DIR="$(LIMA_TARGET_DIR)" cargo build -p libdd-heap-sampler'

lima-test: lima-start
	limactl shell $(LIMA_INSTANCE) sh -lc 'cd "$(CURDIR)" && CARGO_TARGET_DIR="$(LIMA_TARGET_DIR)" cargo test -p libdd-heap-sampler'

lima-shell: lima-start
	limactl shell $(LIMA_INSTANCE)

lima-start:
	@limactl list -q | grep -qx $(LIMA_INSTANCE) || limactl start --name=$(LIMA_INSTANCE) --tty=false $(LIMA_TEMPLATE)

lima-destroy:
	-limactl delete -f $(LIMA_INSTANCE)

clean:
	cargo clean -p libdd-heap-sampler
