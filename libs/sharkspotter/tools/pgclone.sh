#!/bin/bash
#
# Copyright 2020 Joyent, Inc.
# Copyright 2026 Edgecast Cloud LLC.
#
# pgclone.sh -- provision, list, and destroy read-only
# PostgreSQL clone VMs for the Manta rebalancer.
#
# Runs from the Triton headnode.  Single-file, no
# dependencies beyond sdc-* and json(1).
#
# Subcommands:
#
#   pgclone.sh clone-moray <manatee VM UUID>
#   pgclone.sh clone-buckets <buckets-postgres VM UUID>
#   pgclone.sh clone-all --moray-vm <UUID> --buckets-vm <UUID>
#   pgclone.sh list [--type moray|buckets|all] [--json]
#   pgclone.sh destroy <clone VM UUID>
#   pgclone.sh destroy-all [--type moray|buckets]
#
# For backwards compatibility the bare form still works:
#
#   pgclone.sh <manatee VM UUID>
#       (equivalent to: pgclone.sh clone-moray <UUID>)
#
# Invariants (LAW-025):
#   INV-1: Clone VMs tagged with manta_role for discovery
#   INV-2: Failed creation cleans up all artifacts
#   INV-3: Clone never has active recovery.conf
#   INV-4: Autovacuum always disabled in clone
#   INV-5: Unique snapshot name per clone
#
# AI-Generated Code
#

set -o errexit

# -------------------------------------------------------
# Constants
# -------------------------------------------------------
PGCLONE_TAG_MORAY="rebalancer-pg-clone"
PGCLONE_TAG_BUCKETS="rebalancer-buckets-pg-clone"

# -------------------------------------------------------
# Shared helpers
# -------------------------------------------------------

#
# check_result -- validate sdc-oneachnode JSON result.
#
# Arguments:
#   $1  JSON result string from sdc-oneachnode -J
#
# Returns non-zero if the remote command failed.
#
function check_result {
    local result_json="$1"
    local stdout stderr

    stdout="$(json stdout <<<"${result_json}")"
    stderr="$(json stderr <<<"${result_json}")"

    [[ -n ${stdout} ]] && echo "STDOUT: $stdout"
    [[ -n ${stderr} ]] && echo "STDERR: $stderr"
    if [[ $(json exit_status <<<"${result_json}") -ne 0 ]]
    then
        echo "Command failed:" >&2
        return 1
    fi
}

#
# validate_victim_vm -- verify VM exists, set globals.
#
# Arguments:
#   $1  VM UUID
#
# Sets globals:
#   VICTIM_JSON   full VMAPI JSON
#   SERVER_UUID   compute node hosting the VM
#
function validate_victim_vm {
    local victim_uuid="$1"

    VICTIM_JSON="$(sdc-vmapi "/vms/${victim_uuid}" \
        | json -H)"
    SERVER_UUID=$(json server_uuid <<<"${VICTIM_JSON}")

    if [[ -z ${SERVER_UUID} ]]; then
        echo "FATAL: Failed to find server_uuid" \
            "in VM object." >&2
        return 1
    fi
}

#
# create_surrogate_payload -- build a VMAPI create payload
# from the victim VM JSON.
#
# Arguments:
#   $1  new VM UUID
#   $2  new VM alias
#   $3  manta_role tag value
#
# Reads global: VICTIM_JSON
# Outputs: JSON payload on stdout
#
function create_surrogate_payload {
    local new_uuid="$1"
    local new_alias="$2"
    local tag_value="$3"

    json -e 'this.n = {
        autoboot: false,
        billing_id: this.billing_id,
        brand: this.brand,
        cpu_shares: this.cpu_shares,
        customer_metadata: this.customer_metadata,
        delegate_dataset: true,
        dns_domain: this.dns_domain,
        image_uuid: this.image_uuid,
        max_locked_memory: this.max_locked_memory,
        max_lwps: this.max_lwps,
        max_physical_memory: this.max_physical_memory,
        max_swap: this.max_swap,
        owner_uuid: this.owner_uuid,
        quota: this.quota,
        ram: this.ram,
        resolvers: this.resolvers,
        server_uuid: this.server_uuid,
        tags: this.tags,
        tmpfs: this.tmpfs,
        zfs_io_priority: this.zfs_io_priority
    }' \
    -e 'this.n.cpu_cap = 0' \
    -e 'this.n.customer_metadata["user-script"] =
        "#!/usr/bin/bash\n#\n" +
        "set -o xtrace\nset -o errexit\n\n" +
        "if [[ -f /setup.sh ]]; then\n" +
        "\t/setup.sh\nfi\n"' \
    -e "this.n.tags.manta_role = '${tag_value}'" \
    -e "this.n.uuid = '${new_uuid}'" \
    -e "this.n.alias = '${new_alias}'" \
    -e 'this.n.networks = this.nics.map(
        function _mapNic(nic) {
            return {
                ipv4_uuid: nic.network_uuid,
                mtu: nic.mtu,
                nic_tag: nic.nic_tag,
                primary: nic.primary,
                vlan_id: nic.vlan_id
            }
        })' \
    n <<<"${VICTIM_JSON}"
}

#
# provision_vm -- create a VM via VMAPI and wait for the
# provisioning workflow to complete.
#
# Arguments:
#   $1  JSON payload
#
function provision_vm {
    local payload="$1"
    local vmapi_result wf_job_uuid

    vmapi_result=$(sdc-vmapi /vms -X POST \
        -d@- <<<"${payload}" | json -H)
    wf_job_uuid=$(json job_uuid <<<"${vmapi_result}")

    echo "Waiting for workflow ${wf_job_uuid}..."
    sdc-waitforjob -t 600 "${wf_job_uuid}"
}

#
# destroy_delegated_dataset -- remove the empty delegated
# dataset created during provisioning.
#
# Arguments:
#   $1  server UUID
#   $2  new VM UUID
#
function destroy_delegated_dataset {
    local server_uuid="$1" new_uuid="$2"
    local result_json

    echo "Destroying unused delegated dataset..."
    result_json=$(sdc-oneachnode -J \
        -n "${server_uuid}" \
        "zfs destroy zones/${new_uuid}/data" \
        | json result)
    check_result "${result_json}"
}

#
# snapshot_and_clone -- ZFS snapshot the victim Manatee data
# and clone it into the new VM.
#
# Arguments:
#   $1  server UUID
#   $2  victim VM UUID
#   $3  new VM UUID
#   $4  snapshot name
#
function snapshot_and_clone {
    local server_uuid="$1" victim_uuid="$2"
    local new_uuid="$3" snap_name="$4"
    local result_json

    echo "Creating snapshot" \
        "data/manatee@${snap_name}" \
        "on ${victim_uuid}..."
    result_json=$(sdc-oneachnode -J \
        -n "${server_uuid}" \
        "zfs snapshot zones/${victim_uuid}/data/manatee@${snap_name}" \
        | json result)
    check_result "${result_json}"

    echo "Cloning snapshot to" \
        "zones/${new_uuid}/data..."
    result_json=$(sdc-oneachnode -J \
        -n "${server_uuid}" \
        "zfs clone zones/${victim_uuid}/data/manatee@${snap_name} zones/${new_uuid}/data" \
        | json result)
    check_result "${result_json}"
}

#
# copy_registrar_config -- copy Manatee registrar config to
# the new zone for later mutation.
#
# Arguments:
#   $1  server UUID
#   $2  victim VM UUID
#   $3  new VM UUID
#
function copy_registrar_config {
    local server_uuid="$1" victim_uuid="$2"
    local new_uuid="$3"
    local result_json

    echo "Copying registrar config..."
    result_json=$(sdc-oneachnode -J \
        -n "${server_uuid}" \
        "cp /zones/${victim_uuid}/root/opt/smartdc/registrar/etc/config.json /zones/${new_uuid}/root/opt/smartdc/registrar/etc/config.json.in" \
        | json result)
    check_result "${result_json}"
}

#
# generate_setup_script -- write the in-zone setup.sh to a
# temp file and return the path.
#
# Arguments:
#   $1  SMF service name
#   $2  registrar domain regex to validate
#   $3  JS replace expr for aliases
#   $4  JS replace expr for domain
#
# Outputs: path to temp script on stdout
#
function generate_setup_script {
    local smf_service="$1"
    local domain_pattern="$2"
    local alias_replace="$3"
    local domain_replace="$4"

    local tmp_dir="/tmp/rebalancer-pgclone-setup.$$"
    mkdir -p "${tmp_dir}"
    local tmp_script="${tmp_dir}/setup.sh"

    cat >"${tmp_script}" <<SETUP_EOS
#!/bin/bash
#
# Copyright 2020 Joyent, Inc.
# Copyright 2026 Edgecast Cloud LLC.
#
# In-zone setup for pgclone surrogate VM.
# AI-Generated Code
#

set -o xtrace
set -o errexit
set -o pipefail

export PATH=/usr/local/sbin:/usr/local/bin
export PATH=\${PATH}:/opt/local/sbin:/opt/local/bin
export PATH=\${PATH}:/usr/sbin:/usr/bin:/sbin

hostname \$(zonename)
data_dir="/zones/\$(zonename)/data"
pg_version="\$(json current < \${data_dir}/manatee-config.json)"

[[ -z \$(grep "postgres::907" /etc/group) ]] && \
    groupadd -g 907 postgres && \
    useradd -u 907 -g postgres postgres
mkdir -p /var/pg
chown -R postgres:postgres /var/pg

#
# INV-4: Disable autovacuum.
# INV-3: Disable recovery.conf.
#
grep -v "^autovacuum = " \
    \${data_dir}/data/postgresql.conf \
    > \${data_dir}/data/postgresql.conf.new
mv \${data_dir}/data/postgresql.conf.new \
    \${data_dir}/data/postgresql.conf
echo "autovacuum = off" >> \${data_dir}/data/postgresql.conf
[[ -f \${data_dir}/data/recovery.conf ]] && \
    mv \${data_dir}/data/recovery.conf \
       \${data_dir}/data/recovery.conf.disabled

PGBIN="/opt/postgresql/\${pg_version}/bin"
PGDATA="\${data_dir}/data"

cat > \${PGDATA}/pg_hba.conf <<EOF
# TYPE  DATABASE  USER  ADDRESS        METHOD
local   all       all                  trust
local   replication admin              trust
host    all       all   127.0.0.1/32   trust
host    all       all   ::1/128        trust
host    all       all   0.0.0.0/0      trust
EOF

cat > pg.xml <<EOF
<?xml version='1.0'?>
<!DOCTYPE service_bundle SYSTEM '/usr/share/lib/xml/dtd/service_bundle.dtd.1'>
<service_bundle type='manifest' name='export'>
  <service name='${smf_service}' type='service' version='0'>
    <create_default_instance enabled='true'/>
    <single_instance/>
    <dependency name='network'
      grouping='require_all'
      restart_on='none' type='service'>
      <service_fmri
        value='svc:/milestone/network:default'/>
    </dependency>
    <dependency name='filesystem-local'
      grouping='require_all'
      restart_on='none' type='service'>
      <service_fmri
        value='svc:/system/filesystem/local:default'/>
    </dependency>
    <method_context working_directory='/var/pg'>
      <method_credential
        group='postgres' user='postgres'/>
      <method_environment>
        <envvar name='LD_PRELOAD_32'
          value='/usr/lib/extendedFILE.so.1'/>
        <envvar name='PATH'
          value='/opt/local/bin:/opt/local/sbin:/usr/bin:/usr/sbin:/bin:/sbin'/>
      </method_environment>
    </method_context>
    <exec_method name='start' type='method'
      exec='\${PGBIN}/pg_ctl -D \${PGDATA} -l /var/pg/postgresql.log start'
      timeout_seconds='300'/>
    <exec_method name='stop' type='method'
      exec='\${PGBIN}/pg_ctl -D \${PGDATA} stop'
      timeout_seconds='300'/>
    <exec_method name='refresh' type='method'
      exec='\${PGBIN}/pg_ctl -D \${PGDATA} reload'
      timeout_seconds='60'/>
    <template>
      <common_name>
        <loctext xml:lang='C'>
          PostgreSQL RDBMS
        </loctext>
      </common_name>
      <documentation>
        <manpage title='postgres' section='1M'
          manpath='/opt/local/man'/>
        <doc_link name='postgresql.org'
          uri='http://postgresql.org'/>
      </documentation>
    </template>
  </service>
</service_bundle>
EOF

svccfg import pg.xml

MY_IP=\$(mdata-get sdc:nics \
    | json -Ha nic_tag ip \
    | grep "^manta" \
    | cut -d ' ' -f2)
if [[ -z "\${MY_IP}" ]]; then
    echo "Unable to determine Manta IP" >&2
    exit 1
fi

reg_domain=\$(json registration.domain \
    < /opt/smartdc/registrar/etc/config.json.in)
if ! echo "\${reg_domain}" | grep -qE '${domain_pattern}'
then
    echo "Invalid config: bad registration.domain:" \
        "\${reg_domain}" >&2
    exit 1
fi

json -e "this.adminIp = '\${MY_IP}'" \
    -e 'this.registration.aliases =
        [this.registration.domain.replace(
            ${alias_replace})]' \
    -e 'this.registration.domain =
        this.registration.domain.replace(
            ${domain_replace})' \
    > /opt/smartdc/registrar/etc/config.json \
    < /opt/smartdc/registrar/etc/config.json.in

svccfg import \
    /opt/smartdc/registrar/smf/manifests/registrar.xml
svcadm enable registrar

alias="\$(mdata-get sdc:alias)"
if [[ -n \${alias} ]]; then
cat >> /root/.bashrc <<BASHEOF
export PS1="[\u@\${alias} \w]\\\$ "
BASHEOF
fi

SETUP_EOS

    chmod 755 "${tmp_script}"
    echo "${tmp_script}"
}

#
# install_setup_script -- upload setup.sh into the zone.
#
# Arguments:
#   $1  server UUID
#   $2  new VM UUID
#   $3  path to local setup script
#
function install_setup_script {
    local server_uuid="$1" new_uuid="$2"
    local script_path="$3"
    local result_json

    echo "Installing startup script..."
    result_json=$(sdc-oneachnode -X -J \
        -n "${server_uuid}" \
        -g "${script_path}" \
        -d "/zones/${new_uuid}/root" \
        | json result)
    check_result "${result_json}"

    result_json=$(sdc-oneachnode -J \
        -n "${server_uuid}" \
        "chmod 755 /zones/${new_uuid}/root/setup.sh" \
        | json result)
    check_result "${result_json}"
}

#
# start_vm -- start the surrogate VM.
#
# Arguments:
#   $1  server UUID
#   $2  new VM UUID
#
function start_vm {
    local server_uuid="$1" new_uuid="$2"
    local result_json

    echo "Starting Surrogate VM..."
    result_json=$(sdc-oneachnode -J \
        -n "${server_uuid}" \
        "vmadm start ${new_uuid}" \
        | json result)
    check_result "${result_json}"
}

#
# cleanup_on_failure -- idempotent cleanup of a partially
# created clone.  Intended as a trap handler.
#
# Uses globals: NEW_UUID, SERVER_UUID, SNAP_NAME,
#               VICTIM_UUID
#
function cleanup_on_failure {
    echo "" >&2
    echo "ERROR: Clone creation failed." \
        "Cleaning up..." >&2

    if [[ -z ${NEW_UUID:-} ]]; then
        return
    fi

    if [[ -n ${SERVER_UUID:-} ]]; then
        echo "Stopping VM ${NEW_UUID}..." >&2
        sdc-oneachnode -J -n "${SERVER_UUID}" \
            "vmadm stop ${NEW_UUID}" \
            2>/dev/null || true

        echo "Destroying ZFS clone..." >&2
        sdc-oneachnode -J -n "${SERVER_UUID}" \
            "zfs destroy zones/${NEW_UUID}/data" \
            2>/dev/null || true
    fi

    if [[ -n ${SERVER_UUID:-} && \
          -n ${SNAP_NAME:-} && \
          -n ${VICTIM_UUID:-} ]]; then
        echo "Destroying ZFS snapshot..." >&2
        sdc-oneachnode -J -n "${SERVER_UUID}" \
            "zfs destroy zones/${VICTIM_UUID}/data/manatee@${SNAP_NAME}" \
            2>/dev/null || true
    fi

    echo "Deleting VM ${NEW_UUID} via VMAPI..." >&2
    sdc-vmapi "/vms/${NEW_UUID}?action=destroy" \
        -X POST 2>/dev/null || true

    echo "Cleanup complete." >&2
}

# -------------------------------------------------------
# list_clones -- query VMAPI for clone VMs by tag.
#
# Arguments:
#   $1  tag value
#
# Outputs: JSON array on stdout
# -------------------------------------------------------
function list_clones {
    local tag_value="$1"
    sdc-vmapi \
        "/vms?tag.manta_role=${tag_value}&state=active" \
        | json -H
}

# -------------------------------------------------------
# destroy_clone -- full teardown of one clone VM.
#
# Arguments:
#   $1  clone VM UUID
# -------------------------------------------------------
function destroy_clone {
    local clone_uuid="$1"
    local clone_json server_uuid state origin

    clone_json="$(sdc-vmapi "/vms/${clone_uuid}" \
        | json -H)"
    server_uuid=$(json server_uuid <<<"${clone_json}")
    state=$(json state <<<"${clone_json}")

    if [[ -z ${server_uuid} ]]; then
        echo "FATAL: Could not find server_uuid" \
            "for ${clone_uuid}" >&2
        return 1
    fi

    if [[ ${state} == "running" ]]; then
        echo "Stopping VM ${clone_uuid}..."
        sdc-oneachnode -J -n "${server_uuid}" \
            "vmadm stop ${clone_uuid}" \
            2>/dev/null || true
    fi

    echo "Checking ZFS datasets..."
    origin=$(sdc-oneachnode -J -n "${server_uuid}" \
        "zfs get -H -o value origin zones/${clone_uuid}/data" \
        | json result.stdout | tr -d '[:space:]')

    if [[ -n ${origin} && ${origin} != "-" ]]; then
        echo "Destroying ZFS clone" \
            "zones/${clone_uuid}/data..."
        sdc-oneachnode -J -n "${server_uuid}" \
            "zfs destroy zones/${clone_uuid}/data" \
            2>/dev/null || true

        echo "Destroying snapshot ${origin}..."
        sdc-oneachnode -J -n "${server_uuid}" \
            "zfs destroy ${origin}" \
            2>/dev/null || true
    fi

    echo "Deleting VM ${clone_uuid} via VMAPI..."
    sdc-vmapi "/vms/${clone_uuid}?action=destroy" \
        -X POST 2>/dev/null || true

    echo "Destroyed clone ${clone_uuid}."
}

# -------------------------------------------------------
# Subcommand: clone
#
# Arguments:
#   $1  "moray" or "buckets"
#   $2  victim VM UUID
# -------------------------------------------------------
function do_clone {
    local kind="$1"
    local victim_uuid="$2"

    local tag alias_sed_from alias_sed_to
    local smf_service domain_pattern
    local alias_replace domain_replace

    case "${kind}" in
        moray)
            tag="${PGCLONE_TAG_MORAY}"
            alias_sed_from='.postgres.'
            alias_sed_to='.rebalancer-postgres.'
            smf_service="manta/rebalancer-postgres"
            domain_pattern='^[0-9]*\.moray\.'
            alias_replace='/\.moray\./, ".rebalancer-postgres."'
            domain_replace='/^.*\.moray\./, "rebalancer-postgres."'
            ;;
        buckets)
            tag="${PGCLONE_TAG_BUCKETS}"
            alias_sed_from='.buckets-postgres.'
            alias_sed_to='.rebalancer-buckets-postgres.'
            smf_service="manta/rebalancer-buckets-postgres"
            domain_pattern='^[0-9]*\.buckets-postgres\.'
            alias_replace='/\.buckets-postgres\./, ".rebalancer-buckets-postgres."'
            domain_replace='/^.*\.buckets-postgres\./, "rebalancer-buckets-postgres."'
            ;;
        *)
            echo "BUG: unknown kind '${kind}'" >&2
            exit 99
            ;;
    esac

    VICTIM_UUID="${victim_uuid}"
    validate_victim_vm "${VICTIM_UUID}"

    NEW_UUID=$(uuid -v4)
    NEW_UUID_SHORT=$(cut -d'-' -f1 <<<"${NEW_UUID}")
    SNAP_NAME="rebalancer-${NEW_UUID_SHORT}"

    # INV-2: clean up on failure.
    trap cleanup_on_failure ERR

    local js_from
    js_from=$(printf '%s' "${alias_sed_from}" \
        | sed 's/\./\\\\./g')
    local js_to="${alias_sed_to}"

    NEW_ALIAS=$(json -e \
        "this.alias = this.alias
            .replace(/${alias_sed_from}/, '${alias_sed_to}')
            .replace(/-[0-9a-f]+$/, '-${NEW_UUID_SHORT}')" \
        alias <<<"${VICTIM_JSON}")

    echo "Creating Surrogate VM ${NEW_UUID} (${kind})"

    # INV-1: tag for discoverability.
    local new_json
    new_json=$(create_surrogate_payload \
        "${NEW_UUID}" "${NEW_ALIAS}" "${tag}")

    echo "Payload:"
    json <<<"${new_json}"

    provision_vm "${new_json}"

    destroy_delegated_dataset "${SERVER_UUID}" "${NEW_UUID}"
    snapshot_and_clone \
        "${SERVER_UUID}" "${VICTIM_UUID}" \
        "${NEW_UUID}" "${SNAP_NAME}"

    copy_registrar_config \
        "${SERVER_UUID}" "${VICTIM_UUID}" "${NEW_UUID}"

    local setup_script
    setup_script=$(generate_setup_script \
        "${smf_service}" \
        "${domain_pattern}" \
        "${alias_replace}" \
        "${domain_replace}")

    install_setup_script \
        "${SERVER_UUID}" "${NEW_UUID}" "${setup_script}"

    start_vm "${SERVER_UUID}" "${NEW_UUID}"

    echo ""
    echo "Clone created successfully:"
    echo "  Type:        ${kind}"
    echo "  VM UUID:     ${NEW_UUID}"
    echo "  Alias:       ${NEW_ALIAS}"
    echo "  Server:      ${SERVER_UUID}"
    echo "  Snap:        rebalancer-${NEW_UUID_SHORT}"
    echo "  Tag:         ${tag}"
}

# -------------------------------------------------------
# Subcommand: list
# -------------------------------------------------------
function do_list {
    local clone_type="all"
    local output_json=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --type)
                clone_type="$2"; shift 2 ;;
            --json)
                output_json=true; shift ;;
            *)
                echo "Unknown option: $1" >&2
                usage ;;
        esac
    done

    local all_vms="[]"

    function _collect {
        local tag="$1" vms
        vms=$(list_clones "${tag}")
        all_vms=$(json -e \
            "this.m = ${all_vms}.concat(${vms})" \
            m <<<'{}')
    }

    case "${clone_type}" in
        moray)   _collect "${PGCLONE_TAG_MORAY}" ;;
        buckets) _collect "${PGCLONE_TAG_BUCKETS}" ;;
        all)
            _collect "${PGCLONE_TAG_MORAY}"
            _collect "${PGCLONE_TAG_BUCKETS}"
            ;;
        *)
            echo "Unknown type: ${clone_type}" >&2
            usage
            ;;
    esac

    if [[ ${output_json} == true ]]; then
        json <<<"${all_vms}"
        return
    fi

    local count
    count=$(json -a uuid <<<"${all_vms}" 2>/dev/null \
        | wc -l | tr -d ' ')

    if [[ ${count} -eq 0 ]]; then
        echo "No clones found."
        return
    fi

    printf "%-36s  %-40s  %-36s  %-8s  %-20s  %s\n" \
        "UUID" "ALIAS" "SERVER" "STATE" \
        "ROLE" "CREATED"
    printf "%-36s  %-40s  %-36s  %-8s  %-20s  %s\n" \
        "----" "-----" "------" "-----" \
        "----" "-------"

    json -a uuid alias server_uuid state \
        tags.manta_role create_timestamp \
        <<<"${all_vms}" | while read -r line; do
        # shellcheck disable=SC2086
        printf "%-36s  %-40s  %-36s  %-8s  %-20s  %s\n" \
            ${line}
    done

    echo ""
    echo "${count} clone(s) found."
}

# -------------------------------------------------------
# Subcommand: destroy
# -------------------------------------------------------
function do_destroy {
    if [[ -z ${1:-} ]]; then
        echo "Usage: $0 destroy <VM_UUID>" >&2
        exit 2
    fi
    destroy_clone "$1"
}

# -------------------------------------------------------
# Subcommand: destroy-all
# -------------------------------------------------------
function do_destroy_all {
    local clone_type="all"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --type)
                clone_type="$2"; shift 2 ;;
            *)
                echo "Unknown option: $1" >&2
                usage ;;
        esac
    done

    function _destroy_by_tag {
        local tag="$1" vms count
        vms=$(list_clones "${tag}")
        count=$(json -a uuid <<<"${vms}" \
            | wc -l | tr -d ' ')

        if [[ ${count} -eq 0 ]]; then
            echo "No clones for tag ${tag}."
            return
        fi

        echo "Destroying ${count} clone(s)" \
            "for tag ${tag}..."
        json -a uuid <<<"${vms}" \
            | while read -r uuid; do
            destroy_clone "${uuid}"
        done
    }

    case "${clone_type}" in
        moray)   _destroy_by_tag "${PGCLONE_TAG_MORAY}" ;;
        buckets) _destroy_by_tag "${PGCLONE_TAG_BUCKETS}" ;;
        all)
            _destroy_by_tag "${PGCLONE_TAG_MORAY}"
            _destroy_by_tag "${PGCLONE_TAG_BUCKETS}"
            ;;
        *)
            echo "Unknown type: ${clone_type}" >&2
            usage
            ;;
    esac

    echo "Done."
}

# -------------------------------------------------------
# Subcommand: clone-all
# -------------------------------------------------------
function do_clone_all {
    local moray_vm="" buckets_vm=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --moray-vm)
                moray_vm="$2"; shift 2 ;;
            --buckets-vm)
                buckets_vm="$2"; shift 2 ;;
            *)
                echo "Unknown option: $1" >&2
                usage ;;
        esac
    done

    if [[ -z ${moray_vm} && -z ${buckets_vm} ]]; then
        echo "At least one of --moray-vm or" \
            "--buckets-vm required." >&2
        usage
    fi

    local failed=0

    if [[ -n ${moray_vm} ]]; then
        echo "=== Cloning moray Manatee ==="
        if ! do_clone moray "${moray_vm}"; then
            echo "FAILED: moray clone" >&2
            failed=1
        fi
        echo ""
    fi

    if [[ -n ${buckets_vm} ]]; then
        echo "=== Cloning buckets-postgres Manatee ==="
        if ! do_clone buckets "${buckets_vm}"; then
            echo "FAILED: buckets-postgres clone" >&2
            failed=1
        fi
        echo ""
    fi

    if [[ ${failed} -ne 0 ]]; then
        echo "Some clones failed." >&2
        exit 1
    fi

    echo "=== All clones created ==="
    echo ""
    do_list
}

# -------------------------------------------------------
# Usage
# -------------------------------------------------------
function usage {
    cat >&2 <<EOF
Usage:
  $0 clone-moray <manatee VM UUID>
  $0 clone-buckets <buckets-postgres VM UUID>
  $0 clone-all --moray-vm <UUID> --buckets-vm <UUID>
  $0 list [--type moray|buckets|all] [--json]
  $0 destroy <clone VM UUID>
  $0 destroy-all [--type moray|buckets]

  $0 <manatee VM UUID>
      (backwards compat, same as clone-moray)
EOF
    exit 2
}

# -------------------------------------------------------
# Main dispatch
# -------------------------------------------------------
if [[ $# -eq 0 ]]; then
    usage
fi

case "$1" in
    clone-moray)
        shift
        [[ -z ${1:-} ]] && usage
        do_clone moray "$1"
        ;;
    clone-buckets)
        shift
        [[ -z ${1:-} ]] && usage
        do_clone buckets "$1"
        ;;
    clone-all)
        shift
        do_clone_all "$@"
        ;;
    list)
        shift
        do_list "$@"
        ;;
    destroy)
        shift
        do_destroy "$@"
        ;;
    destroy-all)
        shift
        do_destroy_all "$@"
        ;;
    -h|--help)
        usage
        ;;
    -*)
        echo "Unknown option: $1" >&2
        usage
        ;;
    *)
        #
        # Backwards compatibility: bare UUID argument
        # means clone-moray.
        #
        do_clone moray "$1"
        ;;
esac

exit 0
