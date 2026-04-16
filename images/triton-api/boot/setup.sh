#!/bin/bash
#
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at http://mozilla.org/MPL/2.0/.
#
# Copyright 2026 Edgecast Cloud LLC.
#

#
# First-boot setup for the triton-api zone.
#
# Called by the standard Triton user-script (mdata:execute) and also by
# /site/postboot as belt-and-suspenders. Must be idempotent.
#
# The zone runs two services: triton-api-server (loopback) and
# triton-gateway (public). Both read config rendered by config-agent
# from SAPI templates at /opt/smartdc/triton-api/sapi_manifests/.
#

export PS4='[\D{%FT%TZ}] ${BASH_SOURCE}:${LINENO}: ${FUNCNAME[0]:+${FUNCNAME[0]}(): }'
set -o errexit
set -o pipefail
set -o xtrace

SVC_ROOT=/opt/smartdc/triton-api

# Tell sdc_common_setup where to find SAPI manifests for config-agent
CONFIG_AGENT_LOCAL_MANIFESTS_DIRS="${SVC_ROOT}"
source /opt/smartdc/boot/lib/util.sh
sdc_common_setup

# Import the long-running service manifests
/usr/sbin/svccfg import /opt/custom/smf/manifests/triton-api.xml
/usr/sbin/svccfg import /opt/custom/smf/manifests/triton-gateway.xml

# Create the config directory if config-agent hasn't already
mkdir -p ${SVC_ROOT}/etc

sdc_log_rotation_add triton-api-server /var/svc/log/*triton-api*.log 1g
sdc_log_rotation_add triton-gateway /var/svc/log/*triton-gateway*.log 1g
sdc_log_rotation_setup_end

sdc_setup_complete

exit 0
