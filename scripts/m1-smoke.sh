#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd -- "${SCRIPT_DIR}/.." && pwd)

DRY_RUN=${M1_DRY_RUN:-1}
BOOTSTRAP=${M1_BOOTSTRAP:-0}
CLEANUP=${M1_CLEANUP:-0}
SKIP_CN_PROBES=${M1_SKIP_CN_PROBES:-0}
SKIP_GUEST_PROBE=${M1_SKIP_GUEST_PROBE:-0}

TCADM_ENDPOINT=${TCADM_ENDPOINT:-${M1_ENDPOINT:-http://10.199.199.10:8080}}
M1_TENANT_ID=${M1_TENANT_ID:-}
M1_PROJECT_NAME=${M1_PROJECT_NAME:-sandbox}
M1_VPC_NAME=${M1_VPC_NAME:-prod}
M1_VPC_IPV4_BLOCK=${M1_VPC_IPV4_BLOCK:-10.0.0.0/16}
M1_SUBNET_NAME=${M1_SUBNET_NAME:-app}
M1_SUBNET_IPV4_BLOCK=${M1_SUBNET_IPV4_BLOCK:-10.0.1.0/24}
M1_NAT_NAME=${M1_NAT_NAME:-egress}
M1_ROUTE_NAME=${M1_ROUTE_NAME:-default-egress}
M1_ROUTE_DESTINATION=${M1_ROUTE_DESTINATION:-0.0.0.0/0}
M1_INSTANCE_NAME=${M1_INSTANCE_NAME:-web}
M1_INSTANCE_CPU=${M1_INSTANCE_CPU:-2}
M1_INSTANCE_MEMORY_BYTES=${M1_INSTANCE_MEMORY_BYTES:-2147483648}
M1_FIP_NAME=${M1_FIP_NAME:-web-fip}
M1_SSH_KEY_NAME=${M1_SSH_KEY_NAME:-my-key}
M1_SSH_USER=${M1_SSH_USER:-ubuntu}
M1_TENANT_CN_UUID=${M1_TENANT_CN_UUID:-f7d2efb6-8c3b-e1fe-111f-88aedd065474}
M1_EDGE_CN_UUID=${M1_EDGE_CN_UUID:-8b2a9975-6354-8a94-39e4-1c697aa96b33}
M1_TENANT_CN_ROLE=${M1_TENANT_CN_ROLE:-tenant}
M1_EDGE_CN_ROLE=${M1_EDGE_CN_ROLE:-edge}
M1_TENANT_CN_HOST=${M1_TENANT_CN_HOST:-10.199.199.41}
M1_EDGE_CN_HOST=${M1_EDGE_CN_HOST:-10.199.199.40}
M1_LAB_SSH_KEY=${M1_LAB_SSH_KEY:-${HOME}/.ssh/sdc.id_rsa}
M1_SSH_PUBLIC_KEY_FILE=${M1_SSH_PUBLIC_KEY_FILE:-${HOME}/.ssh/id_rsa.pub}
M1_SSH_PRIVATE_KEY=${M1_SSH_PRIVATE_KEY:-}
M1_INSTANCE_TIMEOUT_SECS=${M1_INSTANCE_TIMEOUT_SECS:-300}
M1_NAT_TIMEOUT_SECS=${M1_NAT_TIMEOUT_SECS:-180}
M1_SSH_TIMEOUT_SECS=${M1_SSH_TIMEOUT_SECS:-120}
M1_POLL_INTERVAL_SECS=${M1_POLL_INTERVAL_SECS:-5}
M1_EGRESS_TARGETS=${M1_EGRESS_TARGETS:-https://1.1.1.1 https://9.9.9.9}

TS=$(date -u +"%Y%m%dT%H%M%SZ")
LOG_DIR=${M1_LOG_DIR:-/tmp/triton-vnext-m1-${TS}}
LOG_FILE=${LOG_DIR}/smoke.log
IDS_FILE=${LOG_DIR}/ids.env
TCADM_RESOLVED=

PROJECT_ID=
VPC_ID=
MAIN_ROUTE_TABLE_ID=
SUBNET_ID=
NAT_ID=
ROUTE_ID=
IMAGE_ID=${M1_IMAGE_ID:-}
SSH_KEY_ID=${M1_SSH_KEY_ID:-}
INSTANCE_ID=
NIC_ID=
NIC_IPV4=
FIP_ID=
FIP_ADDRESS=

usage() {
    printf '%s\n' \
        "usage: scripts/m1-smoke.sh [--dry-run|--execute] [--bootstrap] [--cleanup]" \
        "" \
        "Current-tools M1 lab smoke harness. Default is --dry-run." \
        "" \
        "Required for --execute:" \
        "  M1_TENANT_ID=<uuid>             tcadm cannot resolve tenant name yet" \
        "  TCADM_API_KEY=<secret>          or an existing tcadm login/config" \
        "  M1_IMAGE_ID=<uuid>              unless a bhyve linux image is discoverable" \
        "  M1_SSH_KEY_ID=<uuid>            or M1_SSH_PUBLIC_KEY_FILE=<path>" \
        "" \
        "Useful overrides:" \
        "  TCADM_ENDPOINT=${TCADM_ENDPOINT}" \
        "  M1_TENANT_CN_UUID=${M1_TENANT_CN_UUID}" \
        "  M1_EDGE_CN_UUID=${M1_EDGE_CN_UUID}" \
        "  M1_LOG_DIR=${LOG_DIR}" \
        "  M1_SKIP_CN_PROBES=1             skip root@CN proteusadm checks" \
        "  M1_SKIP_GUEST_PROBE=1           skip guest SSH and egress checks"
}

die() {
    printf 'm1-smoke: %s\n' "$*" >&2
    exit 1
}

log() {
    local message=$*
    printf '[%s] %s\n' "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" "${message}" | tee -a "${LOG_FILE}"
}

shell_quote() {
    local arg
    printf '%q' "$1"
    shift || true
    for arg in "$@"; do
        printf ' %q' "${arg}"
    done
}

on_err() {
    local line=$1
    log "failed at line ${line}; logs are in ${LOG_DIR}"
}

init_logs() {
    mkdir -p "${LOG_DIR}"
    : >"${LOG_FILE}"
    : >"${IDS_FILE}"
    log "M1 smoke started"
    log "endpoint=${TCADM_ENDPOINT}"
    log "log_dir=${LOG_DIR}"
}

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --dry-run)
                DRY_RUN=1
                ;;
            --execute)
                DRY_RUN=0
                ;;
            --bootstrap)
                BOOTSTRAP=1
                ;;
            --cleanup)
                CLEANUP=1
                ;;
            --skip-cn-probes)
                SKIP_CN_PROBES=1
                ;;
            --skip-guest-probe)
                SKIP_GUEST_PROBE=1
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                die "unknown argument: $1"
                ;;
        esac
        shift
    done
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

detect_tcadm() {
    if [[ -n "${TCADM_BIN:-}" ]]; then
        printf '%s\n' "${TCADM_BIN}"
    elif [[ -x "${REPO_ROOT}/target/debug/tcadm" ]]; then
        printf '%s\n' "${REPO_ROOT}/target/debug/tcadm"
    elif command -v tcadm >/dev/null 2>&1; then
        command -v tcadm
    else
        printf '%s\n' "__cargo__"
    fi
}

tcadm_base() {
    if [[ "${TCADM_RESOLVED}" == "__cargo__" ]]; then
        printf '%s\n' "cargo run --quiet --manifest-path ${REPO_ROOT}/Cargo.toml -p tcadm --"
    else
        printf '%s\n' "${TCADM_RESOLVED}"
    fi
}

run_tcadm() {
    if [[ "${TCADM_RESOLVED}" == "__cargo__" ]]; then
        cargo run --quiet --manifest-path "${REPO_ROOT}/Cargo.toml" -p tcadm -- --endpoint "${TCADM_ENDPOINT}" "$@"
    else
        "${TCADM_RESOLVED}" --endpoint "${TCADM_ENDPOINT}" "$@"
    fi
}

run_cmd() {
    log "$ $(shell_quote "$@")"
    "$@" 2>&1 | tee -a "${LOG_FILE}"
}

tcadm_json() {
    local outfile=$1
    shift
    log "$ $(shell_quote "$(tcadm_base)") --endpoint $(shell_quote "${TCADM_ENDPOINT}") $(shell_quote "$@") > ${outfile}"
    if run_tcadm "$@" >"${outfile}" 2>>"${LOG_FILE}"; then
        sed 's/^/  /' "${outfile}" >>"${LOG_FILE}"
    else
        log "command failed; stderr appended above"
        return 1
    fi
}

jq_first_id_by_name() {
    local file=$1
    local name=$2
    jq -r --arg name "${name}" 'first(.[] | select(.name == $name) | .id) // empty' "${file}"
}

record_id() {
    local key=$1
    local value=$2
    printf '%s=%q\n' "${key}" "${value}" >>"${IDS_FILE}"
    log "${key}=${value}"
}

assert_nonempty() {
    local label=$1
    local value=$2
    [[ -n "${value}" && "${value}" != "null" ]] || die "expected ${label} to be present"
    log "assert: ${label} present"
}

assert_jq() {
    local file=$1
    local filter=$2
    local label=$3
    if jq -e "${filter}" "${file}" >/dev/null; then
        log "assert: ${label}"
    else
        die "assertion failed: ${label}"
    fi
}

require_execute_inputs() {
    need_cmd jq
    need_cmd sed
    need_cmd ssh

    [[ -n "${M1_TENANT_ID}" ]] || die "set M1_TENANT_ID to the default tenant UUID"
    if [[ -z "${TCADM_API_KEY:-}" && -z "${TCADM_ACCESS_TOKEN:-}" ]]; then
        log "warning: TCADM_API_KEY/TCADM_ACCESS_TOKEN unset; relying on stored tcadm config"
    fi
    if [[ "${SKIP_CN_PROBES}" != "1" && ! -f "${M1_LAB_SSH_KEY}" ]]; then
        die "CN probes need M1_LAB_SSH_KEY=${M1_LAB_SSH_KEY}; pass --skip-cn-probes to bypass"
    fi
}

dry_run_plan() {
    log "dry-run mode: no lab mutations will be made"
    log "would set CN roles:"
    log "  tcadm cn label set ${M1_TENANT_CN_UUID} --role ${M1_TENANT_CN_ROLE} --json"
    log "  tcadm cn label set ${M1_EDGE_CN_UUID} --role ${M1_EDGE_CN_ROLE} --json"
    log "would create or reuse:"
    log "  project ${M1_PROJECT_NAME} under tenant ${M1_TENANT_ID:-<M1_TENANT_ID>}"
    log "  vpc ${M1_VPC_NAME} ${M1_VPC_IPV4_BLOCK}"
    log "  subnet ${M1_SUBNET_NAME} ${M1_SUBNET_IPV4_BLOCK}"
    log "  nat gateway ${M1_NAT_NAME}, route ${M1_ROUTE_DESTINATION} -> nat-gateway:<nat-id>"
    log "  instance ${M1_INSTANCE_NAME}, fip ${M1_FIP_NAME}, ssh-key ${M1_SSH_KEY_NAME}"
    log "would poll instance lifecycle and NAT realized.applied_generation"
    log "would probe proteusadm on ${M1_TENANT_CN_HOST} and guest SSH/FIP unless skipped"
}

maybe_bootstrap() {
    if [[ "${BOOTSTRAP}" == "1" ]]; then
        tcadm_json "${LOG_DIR}/bootstrap.json" bootstrap --json
    else
        log "skipping bootstrap probe; pass --bootstrap to run it"
    fi
}

set_cn_roles() {
    tcadm_json "${LOG_DIR}/cn-list.before.json" cn list --json
    tcadm_json "${LOG_DIR}/cn-tenant-role.json" cn label set "${M1_TENANT_CN_UUID}" --role "${M1_TENANT_CN_ROLE}" --json
    tcadm_json "${LOG_DIR}/cn-edge-role.json" cn label set "${M1_EDGE_CN_UUID}" --role "${M1_EDGE_CN_ROLE}" --json
    assert_jq "${LOG_DIR}/cn-tenant-role.json" ".server_uuid == \"${M1_TENANT_CN_UUID}\" and .role == \"${M1_TENANT_CN_ROLE}\"" "tenant CN role is ${M1_TENANT_CN_ROLE}"
    assert_jq "${LOG_DIR}/cn-edge-role.json" ".server_uuid == \"${M1_EDGE_CN_UUID}\" and .role == \"${M1_EDGE_CN_ROLE}\"" "edge CN role is ${M1_EDGE_CN_ROLE}"
}

ensure_project() {
    local list_file="${LOG_DIR}/projects.list.json"
    local create_file="${LOG_DIR}/project.create.json"
    tcadm_json "${list_file}" tenant project list "${M1_TENANT_ID}" --json
    PROJECT_ID=$(jq_first_id_by_name "${list_file}" "${M1_PROJECT_NAME}")
    if [[ -z "${PROJECT_ID}" ]]; then
        tcadm_json "${create_file}" tenant project create "${M1_TENANT_ID}" --name "${M1_PROJECT_NAME}" --description "M1 smoke project" --json
        PROJECT_ID=$(jq -r '.id' "${create_file}")
    fi
    assert_nonempty "project id" "${PROJECT_ID}"
    record_id PROJECT_ID "${PROJECT_ID}"
}

ensure_vpc() {
    local list_file="${LOG_DIR}/vpcs.list.json"
    local create_file="${LOG_DIR}/vpc.create.json"
    local get_file="${LOG_DIR}/vpc.get.json"
    tcadm_json "${list_file}" tenant project vpc list "${M1_TENANT_ID}" "${PROJECT_ID}" --json
    VPC_ID=$(jq_first_id_by_name "${list_file}" "${M1_VPC_NAME}")
    if [[ -z "${VPC_ID}" ]]; then
        tcadm_json "${create_file}" tenant project vpc create "${M1_TENANT_ID}" "${PROJECT_ID}" --name "${M1_VPC_NAME}" --description "M1 smoke VPC" --ipv4-block "${M1_VPC_IPV4_BLOCK}" --json
        VPC_ID=$(jq -r '.id' "${create_file}")
    fi
    assert_nonempty "vpc id" "${VPC_ID}"
    tcadm_json "${get_file}" tenant project vpc get "${M1_TENANT_ID}" "${PROJECT_ID}" "${VPC_ID}" --json
    MAIN_ROUTE_TABLE_ID=$(jq -r '.main_route_table_id' "${get_file}")
    assert_nonempty "main route table id" "${MAIN_ROUTE_TABLE_ID}"
    record_id VPC_ID "${VPC_ID}"
    record_id MAIN_ROUTE_TABLE_ID "${MAIN_ROUTE_TABLE_ID}"
}

ensure_subnet() {
    local list_file="${LOG_DIR}/subnets.list.json"
    local create_file="${LOG_DIR}/subnet.create.json"
    tcadm_json "${list_file}" tenant project vpc subnet list "${M1_TENANT_ID}" "${PROJECT_ID}" "${VPC_ID}" --json
    SUBNET_ID=$(jq_first_id_by_name "${list_file}" "${M1_SUBNET_NAME}")
    if [[ -z "${SUBNET_ID}" ]]; then
        tcadm_json "${create_file}" tenant project vpc subnet create "${M1_TENANT_ID}" "${PROJECT_ID}" "${VPC_ID}" --name "${M1_SUBNET_NAME}" --description "M1 smoke subnet" --ipv4-block "${M1_SUBNET_IPV4_BLOCK}" --json
        SUBNET_ID=$(jq -r '.id' "${create_file}")
    fi
    assert_nonempty "subnet id" "${SUBNET_ID}"
    record_id SUBNET_ID "${SUBNET_ID}"
}

ensure_nat() {
    local list_file="${LOG_DIR}/nat.list.json"
    local create_file="${LOG_DIR}/nat.create.json"
    tcadm_json "${list_file}" net nat-gw list "${M1_TENANT_ID}" "${PROJECT_ID}" "${VPC_ID}" --json
    NAT_ID=$(jq_first_id_by_name "${list_file}" "${M1_NAT_NAME}")
    if [[ -z "${NAT_ID}" ]]; then
        tcadm_json "${create_file}" net nat-gw create "${M1_TENANT_ID}" "${PROJECT_ID}" "${VPC_ID}" --name "${M1_NAT_NAME}" --description "M1 smoke NAT gateway" --family v4 --json
        NAT_ID=$(jq -r '.id' "${create_file}")
    fi
    assert_nonempty "nat gateway id" "${NAT_ID}"
    record_id NAT_ID "${NAT_ID}"
}

ensure_default_route() {
    local list_file="${LOG_DIR}/routes.list.json"
    local create_file="${LOG_DIR}/route.create.json"
    tcadm_json "${list_file}" net route list "${M1_TENANT_ID}" "${PROJECT_ID}" "${VPC_ID}" "${MAIN_ROUTE_TABLE_ID}" --json
    ROUTE_ID=$(jq -r --arg destination "${M1_ROUTE_DESTINATION}" --arg nat_id "${NAT_ID}" 'first(.[] | select(.destination == $destination and .target.kind == "nat_gateway" and .target.nat_gateway_id == $nat_id) | .id) // empty' "${list_file}")
    if [[ -z "${ROUTE_ID}" ]]; then
        tcadm_json "${create_file}" net route create "${M1_TENANT_ID}" "${PROJECT_ID}" "${VPC_ID}" "${MAIN_ROUTE_TABLE_ID}" --name "${M1_ROUTE_NAME}" --description "M1 smoke default egress" --destination "${M1_ROUTE_DESTINATION}" --target "nat-gateway:${NAT_ID}" --json
        ROUTE_ID=$(jq -r '.id' "${create_file}")
    fi
    assert_nonempty "route id" "${ROUTE_ID}"
    record_id ROUTE_ID "${ROUTE_ID}"
}

ensure_ssh_key() {
    local list_file="${LOG_DIR}/ssh-keys.list.json"
    local create_file="${LOG_DIR}/ssh-key.create.json"
    if [[ -n "${SSH_KEY_ID}" ]]; then
        record_id SSH_KEY_ID "${SSH_KEY_ID}"
        return
    fi
    [[ -f "${M1_SSH_PUBLIC_KEY_FILE}" ]] || die "set M1_SSH_KEY_ID or M1_SSH_PUBLIC_KEY_FILE"
    tcadm_json "${list_file}" tenant project ssh-key list "${M1_TENANT_ID}" "${PROJECT_ID}" --json
    SSH_KEY_ID=$(jq_first_id_by_name "${list_file}" "${M1_SSH_KEY_NAME}")
    if [[ -z "${SSH_KEY_ID}" ]]; then
        tcadm_json "${create_file}" tenant project ssh-key add "${M1_TENANT_ID}" "${PROJECT_ID}" --name "${M1_SSH_KEY_NAME}" --description "M1 smoke SSH key" --public-key-file "${M1_SSH_PUBLIC_KEY_FILE}" --json
        SSH_KEY_ID=$(jq -r '.id' "${create_file}")
    fi
    assert_nonempty "ssh key id" "${SSH_KEY_ID}"
    record_id SSH_KEY_ID "${SSH_KEY_ID}"
}

select_image() {
    local list_file="${LOG_DIR}/images.list.json"
    if [[ -n "${IMAGE_ID}" ]]; then
        record_id IMAGE_ID "${IMAGE_ID}"
        return
    fi
    tcadm_json "${list_file}" tenant project image list "${M1_TENANT_ID}" "${PROJECT_ID}" --json
    IMAGE_ID=$(jq -r 'first(.[] | select(((.os // "") | ascii_downcase | test("linux|ubuntu|debian|alpine|centos|rocky|rhel|fedora")) and (((.compatibility // {}) | .brand // "") == "bhyve")) | .id) // empty' "${list_file}")
    [[ -n "${IMAGE_ID}" ]] || die "no bhyve linux image discovered; set M1_IMAGE_ID"
    record_id IMAGE_ID "${IMAGE_ID}"
}

ensure_instance() {
    local list_file="${LOG_DIR}/instances.list.json"
    local create_file="${LOG_DIR}/instance.create.json"
    tcadm_json "${list_file}" tenant project instance list "${M1_TENANT_ID}" "${PROJECT_ID}" --json
    INSTANCE_ID=$(jq_first_id_by_name "${list_file}" "${M1_INSTANCE_NAME}")
    if [[ -z "${INSTANCE_ID}" ]]; then
        tcadm_json "${create_file}" tenant project instance create "${M1_TENANT_ID}" "${PROJECT_ID}" --name "${M1_INSTANCE_NAME}" --description "M1 smoke instance" --image-id "${IMAGE_ID}" --primary-subnet-id "${SUBNET_ID}" --ssh-key-id "${SSH_KEY_ID}" --cpu "${M1_INSTANCE_CPU}" --memory-bytes "${M1_INSTANCE_MEMORY_BYTES}" --json
        INSTANCE_ID=$(jq -r '.id' "${create_file}")
    fi
    assert_nonempty "instance id" "${INSTANCE_ID}"
    record_id INSTANCE_ID "${INSTANCE_ID}"
}

start_instance_if_stopped() {
    local get_file="${LOG_DIR}/instance.pre-wait.json"
    local start_file="${LOG_DIR}/instance.start.json"
    local state=
    tcadm_json "${get_file}" tenant project instance get "${M1_TENANT_ID}" "${PROJECT_ID}" "${INSTANCE_ID}" --json
    state=$(jq -r '.lifecycle.state' "${get_file}")
    case "${state}" in
        stopped)
            tcadm_json "${start_file}" tenant project instance start "${M1_TENANT_ID}" "${PROJECT_ID}" "${INSTANCE_ID}" --json
            ;;
        failed)
            die "instance ${INSTANCE_ID} is failed; delete it or choose a different M1_INSTANCE_NAME"
            ;;
        *)
            log "instance ${INSTANCE_ID} current lifecycle=${state}"
            ;;
    esac
}

wait_instance_state() {
    local target=$1
    local timeout=$2
    local deadline=$((SECONDS + timeout))
    local file="${LOG_DIR}/instance.poll.json"
    local state=
    while (( SECONDS < deadline )); do
        tcadm_json "${file}" tenant project instance get "${M1_TENANT_ID}" "${PROJECT_ID}" "${INSTANCE_ID}" --json
        state=$(jq -r '.lifecycle.state' "${file}")
        log "instance ${INSTANCE_ID} lifecycle=${state}; waiting for ${target}"
        if [[ "${state}" == "${target}" ]]; then
            log "assert: instance reached ${target}"
            return 0
        fi
        if [[ "${state}" == "failed" ]]; then
            jq -r '.lifecycle.reason // "failed without reason"' "${file}" | tee -a "${LOG_FILE}"
            return 1
        fi
        sleep "${M1_POLL_INTERVAL_SECS}"
    done
    die "timed out waiting for instance ${INSTANCE_ID} to reach ${target}"
}

wait_nat_realized() {
    local deadline=$((SECONDS + M1_NAT_TIMEOUT_SECS))
    local file="${LOG_DIR}/nat.poll.json"
    local desired=
    local applied=
    local edge_cluster_id=
    while (( SECONDS < deadline )); do
        tcadm_json "${file}" net nat-gw get "${M1_TENANT_ID}" "${PROJECT_ID}" "${VPC_ID}" "${NAT_ID}" --json
        desired=$(jq -r '.desired_generation' "${file}")
        applied=$(jq -r '.realized.applied_generation // 0' "${file}")
        edge_cluster_id=$(jq -r '.edge_cluster_id // empty' "${file}")
        log "nat ${NAT_ID} desired=${desired} applied=${applied} edge_cluster_id=${edge_cluster_id:-none}"
        if [[ -n "${edge_cluster_id}" && "${applied}" -ge "${desired}" ]]; then
            record_id EDGE_CLUSTER_ID "${edge_cluster_id}"
            log "assert: NAT desired generation is realized"
            return 0
        fi
        sleep "${M1_POLL_INTERVAL_SECS}"
    done
    die "timed out waiting for NAT realization"
}

ensure_fip() {
    local list_file="${LOG_DIR}/fips.list.json"
    local create_file="${LOG_DIR}/fip.create.json"
    local get_file="${LOG_DIR}/fip.get.json"
    tcadm_json "${list_file}" tenant project floating-ip list "${M1_TENANT_ID}" "${PROJECT_ID}" --json
    FIP_ID=$(jq_first_id_by_name "${list_file}" "${M1_FIP_NAME}")
    if [[ -z "${FIP_ID}" ]]; then
        tcadm_json "${create_file}" tenant project floating-ip create "${M1_TENANT_ID}" "${PROJECT_ID}" --name "${M1_FIP_NAME}" --description "M1 smoke floating IP" --family v4 --json
        FIP_ID=$(jq -r '.id' "${create_file}")
    fi
    assert_nonempty "floating ip id" "${FIP_ID}"
    tcadm_json "${get_file}" tenant project floating-ip get "${M1_TENANT_ID}" "${PROJECT_ID}" "${FIP_ID}" --json
    FIP_ADDRESS=$(jq -r '.address' "${get_file}")
    assert_nonempty "floating ip address" "${FIP_ADDRESS}"
    record_id FIP_ID "${FIP_ID}"
    record_id FIP_ADDRESS "${FIP_ADDRESS}"
}

attach_fip() {
    local nics_file="${LOG_DIR}/nics.list.json"
    local attach_file="${LOG_DIR}/fip.attach.json"
    tcadm_json "${nics_file}" tenant project instance nic list "${M1_TENANT_ID}" "${PROJECT_ID}" "${INSTANCE_ID}" --json
    NIC_ID=$(jq -r 'first(.[] | .id) // empty' "${nics_file}")
    NIC_IPV4=$(jq -r 'first(.[] | .primary_ipv4) // empty' "${nics_file}")
    assert_nonempty "primary nic id" "${NIC_ID}"
    assert_nonempty "primary nic IPv4" "${NIC_IPV4}"
    record_id NIC_ID "${NIC_ID}"
    record_id NIC_IPV4 "${NIC_IPV4}"
    tcadm_json "${attach_file}" tenant project floating-ip attach "${M1_TENANT_ID}" "${PROJECT_ID}" "${FIP_ID}" --nic-id "${NIC_ID}" --json
    assert_jq "${attach_file}" ".attached_to.nic_id == \"${NIC_ID}\"" "floating IP attached to primary NIC"
}

ssh_opts() {
    printf '%s\0' -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile="${LOG_DIR}/known_hosts" -o ConnectTimeout=10
    if [[ -n "${M1_SSH_PRIVATE_KEY}" ]]; then
        printf '%s\0' -i "${M1_SSH_PRIVATE_KEY}"
    elif [[ -f "${M1_SSH_PUBLIC_KEY_FILE%.pub}" ]]; then
        printf '%s\0' -i "${M1_SSH_PUBLIC_KEY_FILE%.pub}"
    fi
}

wait_guest_ssh() {
    local deadline=$((SECONDS + M1_SSH_TIMEOUT_SECS))
    local -a opts=()
    local opt
    while IFS= read -r -d '' opt; do
        opts+=("${opt}")
    done < <(ssh_opts)
    while (( SECONDS < deadline )); do
        log "$ ssh ${M1_SSH_USER}@${FIP_ADDRESS} true"
        if ssh "${opts[@]}" "${M1_SSH_USER}@${FIP_ADDRESS}" true >>"${LOG_FILE}" 2>&1; then
            log "assert: guest SSH reachable at ${FIP_ADDRESS}"
            return 0
        fi
        sleep "${M1_POLL_INTERVAL_SECS}"
    done
    die "timed out waiting for guest SSH at ${FIP_ADDRESS}"
}

probe_guest_egress() {
    local -a opts=()
    local opt
    local target
    while IFS= read -r -d '' opt; do
        opts+=("${opt}")
    done < <(ssh_opts)
    for target in ${M1_EGRESS_TARGETS}; do
        log "$ ssh ${M1_SSH_USER}@${FIP_ADDRESS} curl -fsS --max-time 30 ${target}"
        if ssh "${opts[@]}" "${M1_SSH_USER}@${FIP_ADDRESS}" curl -fsS --max-time 30 "${target}" >>"${LOG_FILE}" 2>&1; then
            log "assert: guest egress succeeded via ${target}"
            return 0
        fi
    done
    die "guest could not reach any egress target: ${M1_EGRESS_TARGETS}"
}

probe_cn_dataplane() {
    local -a opts=(-i "${M1_LAB_SSH_KEY}" -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile="${LOG_DIR}/known_hosts" -o ConnectTimeout=10)
    log "$ ssh root@${M1_TENANT_CN_HOST} proteusadm dump ports"
    ssh "${opts[@]}" "root@${M1_TENANT_CN_HOST}" proteusadm dump ports >"${LOG_DIR}/tenant-cn.ports.txt" 2>>"${LOG_FILE}"
    sed 's/^/  /' "${LOG_DIR}/tenant-cn.ports.txt" >>"${LOG_FILE}"
    if grep -E "${NIC_ID}|${NIC_IPV4}" "${LOG_DIR}/tenant-cn.ports.txt" >/dev/null; then
        log "assert: tenant CN proteus ports mention the smoke NIC"
    else
        die "tenant CN proteus ports did not mention ${NIC_ID} or ${NIC_IPV4}"
    fi

    log "$ ssh root@${M1_TENANT_CN_HOST} proteusadm dump rules"
    ssh "${opts[@]}" "root@${M1_TENANT_CN_HOST}" proteusadm dump rules >"${LOG_DIR}/tenant-cn.rules.txt" 2>>"${LOG_FILE}"
    sed 's/^/  /' "${LOG_DIR}/tenant-cn.rules.txt" >>"${LOG_FILE}"
    if grep -E "${FIP_ADDRESS}|${NIC_IPV4}" "${LOG_DIR}/tenant-cn.rules.txt" >/dev/null; then
        log "assert: tenant CN proteus rules mention FIP or private IP"
    else
        die "tenant CN proteus rules did not mention ${FIP_ADDRESS} or ${NIC_IPV4}"
    fi

    log "$ ssh root@${M1_EDGE_CN_HOST} pgrep -fl fhrun"
    ssh "${opts[@]}" "root@${M1_EDGE_CN_HOST}" pgrep -fl fhrun >"${LOG_DIR}/edge-cn.fhrun.txt" 2>>"${LOG_FILE}" || die "edge CN did not show fhrun"
    sed 's/^/  /' "${LOG_DIR}/edge-cn.fhrun.txt" >>"${LOG_FILE}"
    log "assert: edge CN has fhrun process"
}

cleanup_resources() {
    local cleanup_log="${LOG_DIR}/cleanup.log"
    log "cleanup requested; details also in ${cleanup_log}"
    set +e
    {
        if [[ -n "${INSTANCE_ID}" ]]; then
            run_tcadm tenant project instance stop "${M1_TENANT_ID}" "${PROJECT_ID}" "${INSTANCE_ID}" --json
            wait_instance_state stopped "${M1_INSTANCE_TIMEOUT_SECS}"
            run_tcadm tenant project instance delete "${M1_TENANT_ID}" "${PROJECT_ID}" "${INSTANCE_ID}"
        fi
        if [[ -n "${FIP_ID}" ]]; then
            run_tcadm tenant project floating-ip detach "${M1_TENANT_ID}" "${PROJECT_ID}" "${FIP_ID}" --json
            run_tcadm tenant project floating-ip delete "${M1_TENANT_ID}" "${PROJECT_ID}" "${FIP_ID}"
        fi
        if [[ -n "${ROUTE_ID}" ]]; then
            run_tcadm net route delete "${M1_TENANT_ID}" "${PROJECT_ID}" "${VPC_ID}" "${MAIN_ROUTE_TABLE_ID}" "${ROUTE_ID}"
        fi
        if [[ -n "${NAT_ID}" ]]; then
            run_tcadm net nat-gw delete "${M1_TENANT_ID}" "${PROJECT_ID}" "${VPC_ID}" "${NAT_ID}"
        fi
        if [[ -n "${SUBNET_ID}" ]]; then
            run_tcadm tenant project vpc subnet delete "${M1_TENANT_ID}" "${PROJECT_ID}" "${VPC_ID}" "${SUBNET_ID}"
        fi
        if [[ -n "${VPC_ID}" ]]; then
            run_tcadm tenant project vpc delete "${M1_TENANT_ID}" "${PROJECT_ID}" "${VPC_ID}"
        fi
        if [[ -n "${PROJECT_ID}" ]]; then
            run_tcadm tenant project delete "${M1_TENANT_ID}" "${PROJECT_ID}"
        fi
    } >>"${cleanup_log}" 2>&1
    local status=$?
    set -e
    if [[ "${status}" -eq 0 ]]; then
        log "cleanup finished"
    else
        log "cleanup had failures; inspect ${cleanup_log}"
    fi
}

main() {
    parse_args "$@"
    init_logs
    trap 'on_err $LINENO' ERR

    if [[ "${DRY_RUN}" == "1" ]]; then
        dry_run_plan
        return 0
    fi

    require_execute_inputs
    TCADM_RESOLVED=$(detect_tcadm)
    log "tcadm=$(tcadm_base)"

    maybe_bootstrap
    set_cn_roles
    ensure_project
    ensure_vpc
    ensure_subnet
    ensure_nat
    ensure_default_route
    ensure_ssh_key
    select_image
    ensure_instance
    start_instance_if_stopped
    wait_instance_state running "${M1_INSTANCE_TIMEOUT_SECS}"
    wait_nat_realized
    ensure_fip
    attach_fip

    if [[ "${SKIP_CN_PROBES}" == "1" ]]; then
        log "skipping CN dataplane probes"
    else
        probe_cn_dataplane
    fi

    if [[ "${SKIP_GUEST_PROBE}" == "1" ]]; then
        log "skipping guest SSH/egress probes"
    else
        wait_guest_ssh
        probe_guest_egress
    fi

    if [[ "${CLEANUP}" == "1" ]]; then
        cleanup_resources
    else
        log "leaving smoke resources in place; pass --cleanup to remove them"
    fi

    log "M1 smoke completed"
    log "ids written to ${IDS_FILE}"
}

main "$@"
