#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at http://mozilla.org/MPL/2.0/.
#

#
# Copyright 2020 Joyent, Inc.
# Copyright 2023 MNX Cloud, Inc.
# Copyright 2026 Edgecast Cloud LLC.
#

NAME = rebalancer

include ../image.defs.mk

include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.defs
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.rust.defs
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.smf.defs

BUILD_PLATFORM=20210826T002459Z

ROOT            := $(shell pwd)
LEGACY_DIR       = $(REPO_ROOT)/libs/rebalancer-legacy
RELEASE_TARBALL := $(NAME)-pkg-$(STAMP).tar.gz
AGENT_TARBALL   := $(NAME)-agent-$(STAMP).tar.gz
AGENT_MANIFEST  := $(NAME)-agent-$(STAMP).manifest
RELSTAGEDIR     := /tmp/$(NAME)-$(STAMP)
RELSTAGEDIR_AGENT := /tmp/$(NAME)-agent-$(STAMP)

# Agent is built with a separate target dir to avoid feature unification
# with the manager (which enables the postgres feature via diesel).
AGENT_TARGET_DIR = $(LEGACY_DIR)/target-agent

# Image metadata
BUILDIMAGE_NAME  = mantav2-rebalancer
BUILDIMAGE_DESC  = Manta Rebalancer
BUILDIMAGE_PKG   = $(ROOT)/$(RELEASE_TARBALL)
BUILDIMAGE_PKGSRC = openssl readline sqlite3
AGENTS           = amon config registrar

# PostgreSQL
PGVER = 12.4
DEF_RPATH = /opt/local/lib:/opt/local/gcc7/x86_64-sun-solaris2.11/lib/amd64:/opt/local/gcc7/lib/amd64
REBALANCER_RPATH = /opt/postgresql/$(PGVER)/lib:$(DEF_RPATH)

SMF_MANIFESTS = smf/manifests/rebalancer.xml \
    smf/manifests/rebalancer-agent.xml \
    smf/manifests/postgresql.xml

ifeq ($(shell uname -s),SunOS)
    include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.agent_prebuilt.defs
endif

CLEAN_FILES += bits proto $(NAME)-*.tar.gz $(NAME)-*.manifest

#
# Build targets
#
# The legacy workspace requires swapping Cargo.toml at the repo root.
# Each build target captures the exit status and always restores the
# modern workspace before exiting, even on failure.
#

.PHONY: all
all: manager agent

.PHONY: manager
manager: pg | $(CARGO_EXEC)
	@echo "Building legacy workspace (manager + adm)..."
	@cd $(REPO_ROOT) && \
	    if [ ! -f Cargo.toml.legacy ]; then \
	        echo "error: Cargo.toml.legacy not found at repo root"; exit 1; fi && \
	    mv Cargo.toml Cargo.toml.modern && \
	    mv Cargo.toml.legacy Cargo.toml && \
	    { if [ -f Cargo.lock ]; then mv Cargo.lock Cargo.lock.modern; fi; } && \
	    cp Cargo.lock.legacy Cargo.lock && \
	    LD_LIBRARY_PATH=$(RELSTAGEDIR)/root/opt/postgresql/$(PGVER)/lib:$(DEF_RPATH) \
	    $(CARGO) build --workspace --exclude agent --release; \
	    build_status=$$?; \
	    mv Cargo.toml Cargo.toml.legacy; \
	    mv Cargo.toml.modern Cargo.toml; \
	    rm -f Cargo.lock; \
	    if [ -f Cargo.lock.modern ]; then mv Cargo.lock.modern Cargo.lock; fi; \
	    exit $$build_status
ifeq ($(shell uname -s),SunOS)
	/usr/bin/elfedit -e \
	    "dyn:runpath $(REBALANCER_RPATH)" \
	    $(REPO_ROOT)/target/release/rebalancer-manager
	/usr/bin/elfedit -e \
	    "dyn:runpath $(REBALANCER_RPATH)" \
	    $(REPO_ROOT)/target/release/rebalancer-adm
endif

.PHONY: agent
agent: | $(CARGO_EXEC)
	@echo "Building agent (isolated from workspace)..."
	@cp $(LEGACY_DIR)/agent/Cargo.toml $(LEGACY_DIR)/agent/Cargo.toml.ws
	@sed '/^workspace/d' $(LEGACY_DIR)/agent/Cargo.toml.ws \
	    > $(LEGACY_DIR)/agent/Cargo.toml
	@printf '\n[workspace]\n' >> $(LEGACY_DIR)/agent/Cargo.toml
	@if [ -f $(REPO_ROOT)/Cargo.lock.legacy ]; then \
	    cp $(REPO_ROOT)/Cargo.lock.legacy $(LEGACY_DIR)/agent/Cargo.lock; fi
	STAMP=$(STAMP) $(CARGO) build \
	    --manifest-path=$(LEGACY_DIR)/agent/Cargo.toml \
	    --release --target-dir=$(AGENT_TARGET_DIR) || { \
		mv $(LEGACY_DIR)/agent/Cargo.toml.ws \
		    $(LEGACY_DIR)/agent/Cargo.toml; \
		rm -f $(LEGACY_DIR)/agent/Cargo.lock; exit 1; }
	@mv $(LEGACY_DIR)/agent/Cargo.toml.ws $(LEGACY_DIR)/agent/Cargo.toml
	@rm -f $(LEGACY_DIR)/agent/Cargo.lock

.PHONY: pg
pg: $(REPO_ROOT)/deps/postgresql12/.git
	PATH=/usr/bin:$$PATH $(MAKE) -f Makefile.postgres \
	    RELSTAGEDIR="$(RELSTAGEDIR)" \
	    DEPSDIR="$(REPO_ROOT)/deps" pg12

.PHONY: release
release: all pg $(REPO_ROOT)/deps/manta-scripts/.git $(SMF_MANIFESTS)
	@echo "Building $(RELEASE_TARBALL)"
	# application dir
	@mkdir -p $(RELSTAGEDIR)/root/opt/smartdc/$(NAME)/bin
	@mkdir -p $(RELSTAGEDIR)/root/opt/smartdc/$(NAME)/etc
	@mkdir -p $(RELSTAGEDIR)/root/opt/smartdc/$(NAME)/smf/manifests
	@mkdir -p $(RELSTAGEDIR)/root/opt/smartdc/$(NAME)/smf/methods
	cp etc/postgresql.conf \
	    $(RELSTAGEDIR)/root/opt/smartdc/$(NAME)/etc/
	cp -R sapi_manifests \
	    $(RELSTAGEDIR)/root/opt/smartdc/$(NAME)/
	cp $(REPO_ROOT)/target/release/rebalancer-manager \
	    $(REPO_ROOT)/target/release/rebalancer-adm \
	    $(RELSTAGEDIR)/root/opt/smartdc/$(NAME)/bin/
	cp smf/manifests/rebalancer.xml \
	    $(RELSTAGEDIR)/root/opt/smartdc/$(NAME)/smf/manifests/
	cp smf/manifests/postgresql.xml \
	    $(RELSTAGEDIR)/root/opt/smartdc/$(NAME)/smf/manifests/
	cp smf/methods/postgresql \
	    $(RELSTAGEDIR)/root/opt/smartdc/$(NAME)/smf/methods/
	# boot
	@mkdir -p $(RELSTAGEDIR)/root/opt/smartdc/boot/scripts
	cp -R $(REPO_ROOT)/deps/manta-scripts/*.sh \
	    $(RELSTAGEDIR)/root/opt/smartdc/boot/scripts/
	cp -R boot/* \
	    $(RELSTAGEDIR)/root/opt/smartdc/boot/
	# package it up
	cd $(RELSTAGEDIR) && $(TAR) -I pigz -cf $(ROOT)/$(RELEASE_TARBALL) root
	@rm -rf $(RELSTAGEDIR)

.PHONY: pkg_agent
pkg_agent:
	@echo "Building $(AGENT_TARBALL)"
	@rm -rf $(RELSTAGEDIR_AGENT)
	# agent dir
	@mkdir -p $(RELSTAGEDIR_AGENT)/root/opt/smartdc/$(NAME)-agent/etc
	@mkdir -p $(RELSTAGEDIR_AGENT)/root/opt/smartdc/$(NAME)-agent/bin
	@mkdir -p $(RELSTAGEDIR_AGENT)/root/opt/smartdc/$(NAME)-agent/smf/manifests
	@mkdir -p $(RELSTAGEDIR_AGENT)/root/opt/smartdc/$(NAME)-agent/sapi_manifests
	cp smf/manifests/rebalancer-agent.xml \
	    $(RELSTAGEDIR_AGENT)/root/opt/smartdc/$(NAME)-agent/smf/manifests/
	cp $(AGENT_TARGET_DIR)/release/rebalancer-agent \
	    $(RELSTAGEDIR_AGENT)/root/opt/smartdc/$(NAME)-agent/bin/
	cp -R sapi_manifests/agent \
	    $(RELSTAGEDIR_AGENT)/root/opt/smartdc/$(NAME)-agent/sapi_manifests/
	# package it up
	cd $(RELSTAGEDIR_AGENT) && $(TAR) -I pigz -cf $(ROOT)/$(AGENT_TARBALL) root
	uuid -v4 > $(RELSTAGEDIR_AGENT)/image_uuid
	cat agent-manifest.tmpl | sed \
	    -e "s/UUID/$$(cat $(RELSTAGEDIR_AGENT)/image_uuid)/" \
	    -e "s/NAME/$(BUILDIMAGE_NAME)-agent/" \
	    -e "s/VERSION/$(STAMP)/" \
	    -e "s/DESCRIPTION/$(BUILDIMAGE_DESC) Agent/" \
	    -e "s/SIZE/$$(stat --printf="%s" $(ROOT)/$(AGENT_TARBALL))/" \
	    -e "s/SHA/$$(openssl sha1 $(ROOT)/$(AGENT_TARBALL) \
	    | cut -d ' ' -f2)/" \
	    > $(ROOT)/$(AGENT_MANIFEST)
	@rm -rf $(RELSTAGEDIR_AGENT)

.PHONY: publish
publish: release pkg_agent
	mkdir -p $(ENGBLD_BITS_DIR)/$(NAME)
	cp $(ROOT)/$(RELEASE_TARBALL) $(ENGBLD_BITS_DIR)/$(NAME)/$(RELEASE_TARBALL)
	cp $(ROOT)/$(AGENT_TARBALL) $(ENGBLD_BITS_DIR)/$(NAME)/$(AGENT_TARBALL)
	cp $(ROOT)/$(AGENT_MANIFEST) $(ENGBLD_BITS_DIR)/$(NAME)/$(AGENT_MANIFEST)

#
# Included target definitions.
#
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.deps
ifeq ($(shell uname -s),SunOS)
    include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.agent_prebuilt.targ
endif
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.smf.targ
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.targ
include $(REPO_ROOT)/deps/eng/tools/mk/Makefile.rust.targ
