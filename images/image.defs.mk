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

# Point TOP at the repo root so eng Makefiles find deps/eng,
# rust-toolchain.toml, etc. correctly from images/<service>/.
REPO_ROOT := $(shell git rev-parse --show-toplevel)
ifeq ($(REPO_ROOT),)
$(error git rev-parse --show-toplevel failed. Are you running inside a git repository?)
endif
TOP = $(REPO_ROOT)

# All images use the same eng submodule
ENGBLD_REQUIRE := $(shell git submodule update --init $(REPO_ROOT)/deps/eng)
ifeq ($(wildcard $(REPO_ROOT)/deps/eng/tools/mk/Makefile.defs),)
$(error Failed to initialize deps/eng submodule. Check network and .gitmodules.)
endif

# Default origin image: triton-origin-x86_64-24.4.1
BASE_IMAGE_UUID ?= 41bd4100-eb86-409a-85b0-e649aadf6f62

# Use rustup-managed toolchain, not illumos bootstrap tarballs.
# Must be set before including Makefile.rust.defs (matches root Makefile).
RUST_USE_BOOTSTRAP = false

# Keep build output in the per-image directory (e.g. images/triton-api/bits/)
# so Jenkins jobs can cd into the image dir and run make directly.
ENGBLD_BITS_DIR = $(shell pwd)/bits

# Common buildimage settings
ENGBLD_USE_BUILDIMAGE = true
BUILD_PLATFORM = 20210826T002459Z

# Use the local copy of buildimage from deps/eng
ENGBLD_FORCE_LOCAL_BUILDIMAGE = true
