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
