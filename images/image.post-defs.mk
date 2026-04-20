#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at http://mozilla.org/MPL/2.0/.
#

#
# Copyright 2026 Edgecast Cloud LLC.
#

#
# Shared settings applied after all eng Makefile.*.defs includes. Two kinds
# of thing belong here:
#
#   * Overrides for variables that eng's Makefile.defs unconditionally
#     reassigns (today: BUILD_PLATFORM).
#
#   * Rules that reference variables defined by later eng includes — notably
#     $(CARGO_EXEC), which Makefile.rust.defs sets. An order-only prereq
#     like `| $(CARGO_EXEC)` is expanded when the rule is read, so the rule
#     has to land AFTER rust.defs has defined the variable.
#
# Include from each images/<service>/Makefile AFTER all eng *.defs files
# (Makefile.defs, Makefile.agent_prebuilt.defs, Makefile.rust.defs, etc.).
# Anything that can be set safely BEFORE eng's Makefile.defs belongs in
# image.defs.mk instead.
#

# Our Jenkins builders run on 20210826T002459Z. Makefile.defs hardcodes
# 20181206T011455Z, which makes validate-buildenv reject modern builders.
BUILD_PLATFORM = 20210826T002459Z

#
# Shared release-build helper. Per-image Makefiles set CARGO_PKGS to the
# space-separated list of Cargo package names they need and then make their
# own `release_build` target depend on `_image-cargo-build`. Carries the
# order-only dep on $(CARGO_EXEC) so CI hosts auto-install the Rust
# toolchain before cargo runs — easy to forget when writing a new image
# Makefile by hand (see c394a1f).
#
# Example per-image usage:
#
#     CARGO_PKGS = foo bar
#
#     .PHONY: release_build
#     release_build: _image-cargo-build
#         <any post-build recipes, e.g. elfedit RPATH stripping>
#
.PHONY: _image-cargo-build
_image-cargo-build: | $(CARGO_EXEC)
	cd $(REPO_ROOT) && \
	    for pkg in $(CARGO_PKGS); do \
	        $(CARGO) build --release -p $$pkg; \
	    done
