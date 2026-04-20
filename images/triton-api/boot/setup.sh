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

# Generate the JWT signing keypair (ES256) if one isn't already on disk.
# The private key is the ONLY place tritonapi can sign tokens from; the
# public key is published via /v1/auth/jwks.json for any verifier that
# needs to accept tritonapi-issued JWTs (triton-gateway, future adminui
# proxy, etc.). Both live on the delegated dataset so they survive zone
# reprovisioning.
#
# The private key must be in PKCS#8 PEM format ("-----BEGIN PRIVATE KEY-----")
# because that is what jsonwebtoken's `from_ec_pem` accepts. `openssl genpkey`
# emits PKCS#8 by default; the older `openssl ecparam -genkey` emits SEC1
# ("-----BEGIN EC PRIVATE KEY-----") which the library rejects, so if an old
# key happens to be on disk we regenerate instead of trusting it.
if [[ -f /data/jwt-private.pem && -f /data/jwt-public.pem ]] \
    && /opt/local/bin/openssl pkey -in /data/jwt-private.pem -noout 2>/dev/null \
    && head -1 /data/jwt-private.pem | grep -q 'BEGIN PRIVATE KEY'; then
    echo "JWT signing keypair already present at /data/ (PKCS#8)"
else
    echo "Generating ES256 (P-256) JWT signing keypair at /data/"
    /opt/local/bin/openssl genpkey -algorithm EC \
        -pkeyopt ec_paramgen_curve:P-256 \
        -pkeyopt ec_param_enc:named_curve \
        -out /data/jwt-private.pem
    /opt/local/bin/openssl pkey -in /data/jwt-private.pem \
        -pubout -out /data/jwt-public.pem
    chmod 0400 /data/jwt-private.pem
    chmod 0444 /data/jwt-public.pem
fi

# Sanity-check that config-agent has rendered the CloudAPI signer key.
# The keypair is generated and registered by `tritonadm post-setup
# tritonapi` on the headnode, stored in the triton-api service's SAPI
# metadata, and rendered here by config-agent. If the file is missing
# the operator hasn't run the bootstrap — fail loudly rather than let
# triton-gateway come up without an operator key (it would 502 every
# CloudAPI request).
SIGNER_KEY=/opt/smartdc/triton-gateway/etc/cloudapi-signer-key.pem
if [[ ! -s ${SIGNER_KEY} ]]; then
    echo "ERROR: ${SIGNER_KEY} missing or empty." >&2
    echo "       Run 'tritonadm post-setup tritonapi' on the headnode" >&2
    echo "       to seed the CloudAPI signer key in SAPI." >&2
    exit 1
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
