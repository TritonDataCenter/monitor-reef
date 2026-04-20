#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at http://mozilla.org/MPL/2.0/.
#

#
# Copyright 2026 Edgecast Cloud LLC.
#

#
# images/tritonadm — builds the tritonadm GZ tool tarball + IMGAPI manifest
# for upload to updates.tritondatacenter.com.
#
# Unlike images/triton-api/, this is NOT a zone image. tritonadm runs in the
# headnode global zone, so we ship a plain tarball + IMGAPI 'other'-type
# manifest. See docs/design/tritonadm-distribution.md.
#

NAME = tritonadm

include ../image.defs.mk

# This is a tool, not a zone image — skip buildimage.
ENGBLD_USE_BUILDIMAGE := false

include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.defs

# We don't need the rest of the eng Makefile chain (Makefile.rust.defs pulls
# in toolchain bootstrap; Makefile.targ adds buildimage targets we don't use).
# Define the small subset of tools we actually invoke.
CARGO ?= cargo
TAR   ?= tar

ROOT            := $(shell pwd)
RELSTAGEDIR     := $(ROOT)/proto
RELEASE_TARBALL := tritonadm-$(STAMP).tgz
MANIFEST        := tritonadm-$(STAMP).imgmanifest

# Cargo target output (workspace-shared)
CARGO_BIN := $(REPO_ROOT)/target/release/tritonadm

# Where the tarball stages files. The on-disk layout in the tarball mirrors
# what the install script extracts onto the headnode GZ.
INSTALL_PREFIX := root/opt/triton/tritonadm

# The shells we generate completions for. clap_complete supports more
# (elvish, powershell), but these three cover real operators.
COMPLETION_SHELLS := bash zsh fish

CLEAN_FILES += bits proto $(RELEASE_TARBALL) $(MANIFEST)

#
# Detect build host. Stripping RPATH/RUNPATH only works on illumos with
# /usr/bin/elfedit, and only matters for binaries that will run there. On
# macOS/Linux dev hosts we still produce a tarball, but mark it -dev so
# nobody confuses it with a shippable artifact.
#
UNAME_S := $(shell uname -s)
ifeq ($(UNAME_S),SunOS)
    IS_ILLUMOS := true
else
    IS_ILLUMOS := false
endif

.PHONY: all
all: release_build

.PHONY: release_build
release_build:
	cd $(REPO_ROOT) && $(CARGO) build --release -p tritonadm
ifeq ($(IS_ILLUMOS),true)
	/usr/bin/elfedit -e 'dyn:delete RUNPATH' $(CARGO_BIN) || true
	/usr/bin/elfedit -e 'dyn:delete RPATH' $(CARGO_BIN) || true
else
	@echo "WARNING: not on illumos — RPATH/RUNPATH not stripped."
	@echo "         Resulting tarball is for local testing only."
endif

.PHONY: release
release: all
	@echo "Staging $(RELEASE_TARBALL)"
	@rm -rf $(RELSTAGEDIR)
	@mkdir -p $(RELSTAGEDIR)/$(INSTALL_PREFIX)/bin
	@mkdir -p $(RELSTAGEDIR)/$(INSTALL_PREFIX)/etc
	@mkdir -p $(RELSTAGEDIR)/$(INSTALL_PREFIX)/share/completions
	cp $(CARGO_BIN) $(RELSTAGEDIR)/$(INSTALL_PREFIX)/bin/tritonadm
	chmod 755 $(RELSTAGEDIR)/$(INSTALL_PREFIX)/bin/tritonadm
	@echo "$(STAMP)" > $(RELSTAGEDIR)/$(INSTALL_PREFIX)/etc/version
	@for shell in $(COMPLETION_SHELLS); do \
	    $(CARGO_BIN) completion $$shell \
	        > $(RELSTAGEDIR)/$(INSTALL_PREFIX)/share/completions/tritonadm.$$shell \
	        || { echo "completion $$shell failed"; exit 1; }; \
	done
	(cd $(RELSTAGEDIR) && $(TAR) -czf $(ROOT)/$(RELEASE_TARBALL) root)
	@echo "Generating $(MANIFEST)"
	$(ROOT)/scripts/make-manifest.sh \
	    --tarball $(ROOT)/$(RELEASE_TARBALL) \
	    --version $(STAMP) \
	    --branch $(BRANCH) \
	    > $(ROOT)/$(MANIFEST)
	@echo ""
	@echo "Built:"
	@echo "  $(ROOT)/$(RELEASE_TARBALL)"
	@echo "  $(ROOT)/$(MANIFEST)"

.PHONY: publish
publish: release
	@mkdir -p $(ENGBLD_BITS_DIR)/$(NAME)
	cp $(ROOT)/$(RELEASE_TARBALL) $(ENGBLD_BITS_DIR)/$(NAME)/$(RELEASE_TARBALL)
	cp $(ROOT)/$(MANIFEST) $(ENGBLD_BITS_DIR)/$(NAME)/$(MANIFEST)
	@echo "Published bits to $(ENGBLD_BITS_DIR)/$(NAME)/"

#
# bits-upload — push the staged bits to updates.tritondatacenter.com via the
# standard eng tooling. Requires updates-imgadm credentials in the environment
# (see deps/eng/tools/bits-upload.sh).
#
.PHONY: bits-upload
bits-upload: publish
	$(REPO_ROOT)/deps/eng/tools/bits-upload.sh \
	    -p \
	    -b $(BRANCH) \
	    -t $(TIMESTAMP) \
	    -d $(ENGBLD_DEST_OUT_PATH) \
	    -n $(NAME) \
	    $(NAME)

.PHONY: clean
clean:
	rm -rf $(RELSTAGEDIR) bits $(RELEASE_TARBALL) $(MANIFEST)
