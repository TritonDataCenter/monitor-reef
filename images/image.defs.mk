#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at http://mozilla.org/MPL/2.0/.
#

#
# Copyright 2026 Edgecast Cloud LLC.
#

#
# Shared definitions for all zone image builds in this monorepo.
# Each images/<service>/Makefile includes this before the eng Makefiles.
#

# Tell eng Makefiles where the repo root is, so they find deps/eng,
# rust-toolchain.toml, etc. correctly even though $(TOP) resolves to
# images/<service>/.
ENGBLD_REPO_ROOT := $(shell git rev-parse --show-toplevel)
ifeq ($(ENGBLD_REPO_ROOT),)
$(error git rev-parse --show-toplevel failed. Are you running inside a git repository?)
endif

# All images use the same eng submodule
ENGBLD_REQUIRE := $(shell git submodule update --init $(ENGBLD_REPO_ROOT)/deps/eng)
ifeq ($(wildcard $(ENGBLD_REPO_ROOT)/deps/eng/tools/mk/Makefile.defs),)
$(error Failed to initialize deps/eng submodule. Check network and .gitmodules.)
endif

# Default origin image: triton-origin-x86_64-24.4.1
BASE_IMAGE_UUID ?= 41bd4100-eb86-409a-85b0-e649aadf6f62

# Common buildimage settings
ENGBLD_USE_BUILDIMAGE = true
BUILD_PLATFORM = 20210826T002459Z

# Use the local copy of buildimage from deps/eng
ENGBLD_FORCE_LOCAL_BUILDIMAGE = true
