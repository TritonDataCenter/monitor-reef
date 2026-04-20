#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at http://mozilla.org/MPL/2.0/.
#

#
# Copyright 2026 Edgecast Cloud LLC.
#

NAME = triton-api
DIR_NAME = triton-api

include ../image.defs.mk
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.defs
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.agent_prebuilt.defs
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.rust.defs
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.smf.defs
# Must come AFTER Makefile.rust.defs because image.post-defs.mk's
# _image-cargo-build rule carries a `| $(CARGO_EXEC)` order-only dep,
# and CARGO_EXEC isn't set until rust.defs runs.
include ../image.post-defs.mk

# Triton agents bundled into the image by buildimage
AGENTS = config registrar

ROOT            := $(shell pwd)
RELEASE_TARBALL := $(NAME)-pkg-$(STAMP).tar.gz
RELSTAGEDIR     := $(ROOT)/proto

SMF_MANIFESTS = smf/manifests/postboot.xml smf/manifests/triton-api.xml \
    smf/manifests/triton-gateway.xml smf/manifests/haproxy.xml

# Image metadata
BUILDIMAGE_NAME = $(NAME)
BUILDIMAGE_DESC = Triton API service
BUILDIMAGE_PKG = $(ROOT)/$(RELEASE_TARBALL)
BUILDIMAGE_PKGSRC = \
    haproxy-3.2.3

# Cargo package names for the binaries (both run in the same zone)
CARGO_PKGS = triton-api-server triton-gateway

CLEAN_FILES += bits proto target

.PHONY: release_build
release_build: _image-cargo-build

.PHONY: all
all: release_build

.PHONY: release
release: all
	@echo "Building $(RELEASE_TARBALL)"
	@rm -rf $(RELSTAGEDIR)
	@mkdir -p $(RELSTAGEDIR)/root/opt/smartdc/boot
	@mkdir -p $(RELSTAGEDIR)/root/opt/smartdc/$(DIR_NAME)
	@mkdir -p $(RELSTAGEDIR)/root/opt/custom/smf
	@mkdir -p $(RELSTAGEDIR)/root/opt/smartdc/$(DIR_NAME)/sapi_manifests
	@mkdir -p $(RELSTAGEDIR)/site
	@touch $(RELSTAGEDIR)/site/.do-not-delete-me
	for bin in $(CARGO_PKGS); do \
	    cp $(REPO_ROOT)/target/release/$$bin \
	        $(RELSTAGEDIR)/root/opt/smartdc/$(DIR_NAME)/; \
	done
	cp -r $(REPO_ROOT)/deps/sdc-scripts/{etc,lib,sbin,smf} \
	    $(RELSTAGEDIR)/root/opt/smartdc/boot/
	cp -PR boot/* $(RELSTAGEDIR)/root/opt/smartdc/boot/
	cp -PR smf/* $(RELSTAGEDIR)/root/opt/custom/smf/
	cp -PR sapi_manifests/* \
	    $(RELSTAGEDIR)/root/opt/smartdc/$(DIR_NAME)/sapi_manifests/
	(cd $(RELSTAGEDIR) && $(TAR) -I pigz -cf $(ROOT)/$(RELEASE_TARBALL) root site)

.PHONY: publish
publish: release
	mkdir -p $(ENGBLD_BITS_DIR)/$(NAME)
	cp $(ROOT)/$(RELEASE_TARBALL) $(ENGBLD_BITS_DIR)/$(NAME)/$(RELEASE_TARBALL)

include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.deps
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.agent_prebuilt.targ
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.rust.targ
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.smf.targ
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.targ
