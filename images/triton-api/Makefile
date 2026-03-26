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
include $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.defs
include $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.rust.defs
include $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.smf.defs

ROOT            := $(shell pwd)
RELEASE_TARBALL := $(NAME)-pkg-$(STAMP).tar.gz
RELSTAGEDIR     := $(ROOT)/proto

SMF_MANIFESTS = smf/manifests/postboot.xml smf/manifests/triton-api.xml

# Image metadata
BUILDIMAGE_NAME = $(NAME)
BUILDIMAGE_DESC = Triton API service
BUILDIMAGE_PKGSRC =

# Cargo package name for the binary
SERVICE_BINS = triton-api-server

CLEAN_FILES += bits proto target

.PHONY: release_build
release_build:
	cd $(ENGBLD_REPO_ROOT) && $(CARGO) build --release -p $(SERVICE_BINS)

.PHONY: all
all: release_build

.PHONY: release
release: all
	@echo "Building $(RELEASE_TARBALL)"
	@rm -rf $(RELSTAGEDIR)
	@mkdir -p $(RELSTAGEDIR)/root/opt/triton/boot
	@mkdir -p $(RELSTAGEDIR)/root/opt/triton/$(DIR_NAME)
	@mkdir -p $(RELSTAGEDIR)/root/opt/custom/smf
	@mkdir -p $(RELSTAGEDIR)/root/opt/smartdc/$(DIR_NAME)/sapi_manifests
	@mkdir -p $(RELSTAGEDIR)/site
	@touch $(RELSTAGEDIR)/site/.do-not-delete-me
	cp $(ENGBLD_REPO_ROOT)/target/release/$(SERVICE_BINS) \
	    $(RELSTAGEDIR)/root/opt/triton/$(DIR_NAME)/
	cp -PR boot/* $(RELSTAGEDIR)/root/opt/triton/boot/
	cp -PR smf/* $(RELSTAGEDIR)/root/opt/custom/smf/
	cp -PR sapi_manifests/* \
	    $(RELSTAGEDIR)/root/opt/smartdc/$(DIR_NAME)/sapi_manifests/
	(cd $(RELSTAGEDIR) && $(TAR) -I pigz -cf $(ROOT)/$(RELEASE_TARBALL) root site)

.PHONY: publish
publish: release
	mkdir -p $(ENGBLD_BITS_DIR)/$(NAME)
	cp $(ROOT)/$(RELEASE_TARBALL) $(ENGBLD_BITS_DIR)/$(NAME)/$(RELEASE_TARBALL)

include $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.deps
include $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.rust.targ
include $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.smf.targ
include $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.targ
