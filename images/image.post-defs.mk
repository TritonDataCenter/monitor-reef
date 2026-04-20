#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at http://mozilla.org/MPL/2.0/.
#

#
# Copyright 2026 Edgecast Cloud LLC.
#

#
# Shared settings that must be applied AFTER eng's Makefile.defs, typically
# because Makefile.defs unconditionally reassigns a variable and our value
# would otherwise be silently stomped.
#
# Each images/<service>/Makefile should include this immediately after it
# includes $(REPO_ROOT)/deps/eng/tools/mk/Makefile.defs. Anything that can
# be set safely BEFORE Makefile.defs belongs in image.defs.mk instead.
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
