# Shared Lima infrastructure for libdd-heap-* crates.
# Include this file from a crate Makefile with: include ../heap.mk

LIMA_INSTANCE_ARM64 := libdd-heap-sampler-arm64
LIMA_INSTANCE_AMD64 := libdd-heap-sampler-amd64

LIMA_TARGET_DIR     := $$HOME/libdatadog-target
LIMA_WORKSPACE_ROOT := $(abspath $(CURDIR)/..)

LIMA_TEMPLATE_ARM64 := $(abspath $(CURDIR)/../lima-arm64.yaml)
LIMA_TEMPLATE_AMD64 := $(abspath $(CURDIR)/../lima-amd64.yaml)

.PHONY: lima-start-arm64 lima-start-amd64 lima-shell lima-destroy

lima-start-arm64:
	@limactl list -q | grep -qx $(LIMA_INSTANCE_ARM64) || \
	  limactl start --name=$(LIMA_INSTANCE_ARM64) --tty=false $(LIMA_TEMPLATE_ARM64)

lima-start-amd64:
	@limactl list -q | grep -qx $(LIMA_INSTANCE_AMD64) || \
	  limactl start --name=$(LIMA_INSTANCE_AMD64) --tty=false $(LIMA_TEMPLATE_AMD64)

lima-shell: lima-start-arm64
	limactl shell $(LIMA_INSTANCE_ARM64)

lima-destroy:
	-limactl delete -f $(LIMA_INSTANCE_ARM64)
	-limactl delete -f $(LIMA_INSTANCE_AMD64)
