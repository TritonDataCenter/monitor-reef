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
# Must come AFTER Makefile.defs (which hardcodes the old 20181206 default);
# see image.defs.mk for the full explanation.
BUILD_PLATFORM = 20210826T002459Z
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.agent_prebuilt.defs
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.rust.defs
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.smf.defs

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
SERVICE_BINS = triton-api-server triton-gateway

CLEAN_FILES += bits proto target

.PHONY: release_build
release_build: | $(CARGO_EXEC)
	cd $(REPO_ROOT) && \
	    for pkg in $(SERVICE_BINS); do \
	        $(CARGO) build --release -p $$pkg; \
	    done

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
	for bin in $(SERVICE_BINS); do \
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
