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

# Pull in eng's Rust toolchain management so CI hosts auto-install rustup
# on first build. $(CARGO) and $(CARGO_EXEC) come from here; any recipe that
# invokes $(CARGO) must carry an order-only dep on | $(CARGO_EXEC) so the
# toolchain is fetched before cargo runs (see comment in Makefile.rust.defs).
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.rust.defs
# Must come AFTER Makefile.rust.defs because image.post-defs.mk's
# _image-cargo-build rule carries a `| $(CARGO_EXEC)` order-only dep,
# and CARGO_EXEC isn't set until rust.defs runs.
include ../image.post-defs.mk

TAR ?= tar

ROOT            := $(shell pwd)
RELSTAGEDIR     := $(ROOT)/proto
RELEASE_TARBALL := tritonadm-$(STAMP).tgz
MANIFEST        := tritonadm-$(STAMP).imgmanifest
UUID_FILE       := $(ROOT)/$(STAMP).uuid

# Cargo packages to build. Consumed by _image-cargo-build in image.post-defs.mk.
CARGO_PKGS = tritonadm

# Cargo target output (workspace-shared)
CARGO_BIN := $(REPO_ROOT)/target/release/tritonadm

# Where the tarball stages files. The on-disk layout in the tarball mirrors
# what the install script extracts onto the headnode GZ.
INSTALL_PREFIX := root/opt/triton/tritonadm

# The shells we generate completions for. clap_complete supports more
# (elvish, powershell), but these three cover real operators.
COMPLETION_SHELLS := bash zsh fish

CLEAN_FILES += bits proto $(RELEASE_TARBALL) $(MANIFEST) $(UUID_FILE)

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
release_build: _image-cargo-build
ifeq ($(IS_ILLUMOS),true)
	/usr/bin/elfedit -e 'dyn:delete RUNPATH' $(CARGO_BIN) || true
	/usr/bin/elfedit -e 'dyn:delete RPATH' $(CARGO_BIN) || true
else
	@echo "WARNING: not on illumos — RPATH/RUNPATH not stripped."
	@echo "         Resulting tarball is for local testing only."
endif

# Generate the IMGAPI image UUID once per release so the same UUID can be
# baked into the tarball (etc/version) AND referenced by the manifest. The
# file is per-stamp so reruns of the same build reuse the UUID; clean drops
# it. Required by the embedded install path so /etc/version stays
# round-trippable.
$(UUID_FILE):
	uuidgen | tr '[:upper:]' '[:lower:]' > $@

.PHONY: release
release: all $(UUID_FILE)
	@echo "Staging $(RELEASE_TARBALL)"
	@rm -rf $(RELSTAGEDIR)
	@mkdir -p $(RELSTAGEDIR)/$(INSTALL_PREFIX)/bin
	@mkdir -p $(RELSTAGEDIR)/$(INSTALL_PREFIX)/etc
	@mkdir -p $(RELSTAGEDIR)/$(INSTALL_PREFIX)/share/completions
	cp $(CARGO_BIN) $(RELSTAGEDIR)/$(INSTALL_PREFIX)/bin/tritonadm
	chmod 755 $(RELSTAGEDIR)/$(INSTALL_PREFIX)/bin/tritonadm
	# Bake uuid + version + build_at into etc/version. install-tritonadm.sh
	# in embedded mode reads uuid + version from here; install-time then
	# rewrites etc/version with installed_at + source. See the install
	# script header comment for the canonical format.
	@printf 'uuid=%s\nversion=%s\nbuild_at=%s\n' \
	    "$$(cat $(UUID_FILE))" \
	    "$(STAMP)" \
	    "$$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
	    > $(RELSTAGEDIR)/$(INSTALL_PREFIX)/etc/version
	@for shell in $(COMPLETION_SHELLS); do \
	    $(CARGO_BIN) completion $$shell \
	        > $(RELSTAGEDIR)/$(INSTALL_PREFIX)/share/completions/tritonadm.$$shell \
	        || { echo "completion $$shell failed"; exit 1; }; \
	done
	# Stage install.sh at the tarball root so the tarball is self-installing:
	# extractor untars to a temp dir then runs ./install.sh, mirroring the
	# sdcadm-shar pattern that sdcadm experimental will exec.
	cp $(REPO_ROOT)/tools/install-tritonadm.sh $(RELSTAGEDIR)/install.sh
	chmod 755 $(RELSTAGEDIR)/install.sh
	(cd $(RELSTAGEDIR) && $(TAR) -czf $(ROOT)/$(RELEASE_TARBALL) install.sh root)
	@echo "Generating $(MANIFEST)"
	$(ROOT)/scripts/make-manifest.sh \
	    --tarball $(ROOT)/$(RELEASE_TARBALL) \
	    --uuid $$(cat $(UUID_FILE)) \
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

#
# buildimage is the zone-image step; tritonadm is a tool tarball, not a zone,
# so this is a no-op. Kept as an explicit target because joyBuildImageAndUpload
# in jenkins-joylib unconditionally runs `make ... buildimage ...`.
#
.PHONY: buildimage
buildimage:
	@echo "buildimage: no-op (tritonadm is a tool tarball, not a zone image)"

#
# Generic value-inspection target used by Jenkins: `make print-STAMP` prints
# whatever STAMP resolves to. Mirrors the same facility in eng's Makefile.targ
# which we don't include here.
#
.PHONY: print-%
print-%:
	@echo "$* is $($*)"

# Double-colon so it composes with clean:: in Makefile.rust.targ below.
clean::
	rm -rf $(RELSTAGEDIR) bits $(RELEASE_TARBALL) $(MANIFEST) $(UUID_FILE)

# Brings in the $(CARGO_EXEC): rust-fetch rule that installs rustup on demand.
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.rust.targ
