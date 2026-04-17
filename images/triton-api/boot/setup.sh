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

# Mount the delegated dataset at /data so the TLS cert persists across
# zone reprovisioning (same pattern as sdc-cloudapi's setup.sh).
zfs set mountpoint=/data zones/$(zonename)/data

# Generate a self-signed EC cert for haproxy if one isn't already on disk.
# Clients must set tls_verify=false (or pin this cert) to trust it.
# This mirrors sdc-cloudapi's setup_tls_certificate().
if [[ -f /data/tls/key.pem && -f /data/tls/cert.pem ]]; then
    echo "TLS certificate already present at /data/tls/"
else
    echo "Generating self-signed TLS certificate at /data/tls/"
    mkdir -p /data/tls
    /opt/local/bin/openssl req -x509 -nodes -subj '/CN=*' \
        -pkeyopt ec_paramgen_curve:prime256v1 \
        -pkeyopt ec_param_enc:named_curve \
        -newkey ec -keyout /data/tls/key.pem \
        -out /data/tls/cert.pem -days 3650
    # haproxy expects the private key concatenated after the cert.
    cat /data/tls/key.pem >> /data/tls/cert.pem
fi

# Import the long-running service manifests
/usr/sbin/svccfg import /opt/custom/smf/manifests/triton-api.xml
/usr/sbin/svccfg import /opt/custom/smf/manifests/triton-gateway.xml
/usr/sbin/svccfg import /opt/custom/smf/manifests/haproxy.xml

# Create the config directory if config-agent hasn't already
mkdir -p ${SVC_ROOT}/etc

sdc_log_rotation_add triton-api-server /var/svc/log/*triton-api*.log 1g
sdc_log_rotation_add triton-gateway /var/svc/log/*triton-gateway*.log 1g
sdc_log_rotation_add haproxy /var/svc/log/*haproxy*.log 1g
sdc_log_rotation_setup_end

sdc_setup_complete

exit 0
