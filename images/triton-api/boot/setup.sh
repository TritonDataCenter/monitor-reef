#!/bin/bash
#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at http://mozilla.org/MPL/2.0/.
#

#
# Copyright 2026 Edgecast Cloud LLC.
#

#
# Triton API zone first-boot setup script.
#

export PS4='[\D{%FT%TZ}] ${BASH_SOURCE}:${LINENO}: '\
'${FUNCNAME[0]:+${FUNCNAME[0]}(): }'
set -o xtrace
set -o errexit
set -o pipefail

. /lib/svc/share/smf_include.sh

MARKER=/var/tmp/.first-boot-done

if [[ -f "$MARKER" ]]; then
    echo "Already completed first boot setup."
    exit $SMF_EXIT_OK
fi

# Import the triton-api service manifest
/usr/sbin/svccfg import /opt/custom/smf/manifests/triton-api.xml

touch "$MARKER"
exit $SMF_EXIT_OK
