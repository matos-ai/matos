#!/usr/bin/env bash
set -euo pipefail

: "${FERRISKEY_URL:?FERRISKEY_URL is required}"
: "${REALM:?REALM is required}"
: "${ADMIN_TOKEN:?ADMIN_TOKEN is required}"

AUTHZ_DIR="${AUTHZ_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"
MANAGER="${AUTHZ_MANAGER:-matos}"
NAMESPACE="${AUTHZ_NAMESPACE:-Matos}"
BASE="${FERRISKEY_URL%/}/realms/${REALM}/authz"
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
RESPONSE="${WORK}/response"

request() {
    local method="$1" path="$2" body="${3:-}"
    if [ -n "${body}" ]; then
        STATUS="$(curl -sS -o "${RESPONSE}" -w '%{http_code}' -X "${method}" "${BASE}${path}" \
            -H "Authorization: Bearer ${ADMIN_TOKEN}" -H 'content-type: application/json' -d "${body}")"
    else
        STATUS="$(curl -sS -o "${RESPONSE}" -w '%{http_code}' -X "${method}" "${BASE}${path}" \
            -H "Authorization: Bearer ${ADMIN_TOKEN}")"
    fi
}

succeeded() {
    case "${STATUS}" in
        2??) return 0 ;;
        *)
            echo "${1}: status ${STATUS}: $(cat "${RESPONSE}")" >&2
            exit 1
            ;;
    esac
}

policy_body() {
    jq -n --arg name "$1" --arg source "$2" \
        '{name: $name, enabled: true, body: {type: "static", source: $source}}'
}

request GET "/policies?managed_by=${MANAGER}"
succeeded "list policies"
existing="$(cat "${RESPONSE}")"

wanted=()
for file in "${AUTHZ_DIR}"/policies/*.cedar; do
    wanted+=("$(basename "${file}" .cedar)")
done

while IFS=$'\t' read -r id name; do
    [ -n "${id}" ] || continue
    found=0
    for candidate in "${wanted[@]}"; do
        [ "${candidate}" = "${name}" ] && found=1
    done
    if [ "${found}" = 0 ]; then
        request DELETE "/policies/${id}?managed_by=${MANAGER}"
        succeeded "delete policy ${name}"
        echo "deleted ${name}"
    fi
done < <(jq -r '.[] | [.id, .name] | @tsv' <<<"${existing}")

schema_source="$(cat "${AUTHZ_DIR}/schema.cedarschema")"
request GET /schema
if [ "${STATUS}" = 200 ] \
    && [ "$(jq -r .namespace "${RESPONSE}")" = "${NAMESPACE}" ] \
    && [ "$(jq -r .source "${RESPONSE}")" = "${schema_source}" ]; then
    echo "unchanged schema"
else
    request PUT /schema "$(jq -n --arg namespace "${NAMESPACE}" --arg source "${schema_source}" \
        '{namespace: $namespace, source: $source}')"
    succeeded "replace schema"
    echo "replaced schema"
fi

for file in "${AUTHZ_DIR}"/policies/*.cedar; do
    name="$(basename "${file}" .cedar)"
    source="$(cat "${file}")"
    current="$(jq -c --arg name "${name}" '[.[] | select(.name == $name)][0] // empty' <<<"${existing}")"
    if [ -z "${current}" ]; then
        request POST "/policies?managed_by=${MANAGER}" "$(policy_body "${name}" "${source}")"
        succeeded "create policy ${name}"
        echo "created ${name}"
    elif [ "$(jq -r '.enabled' <<<"${current}")" = true ] \
        && [ "$(jq -r '.body.source' <<<"${current}")" = "${source}" ]; then
        echo "unchanged ${name}"
    else
        request PUT "/policies/$(jq -r .id <<<"${current}")?managed_by=${MANAGER}" "$(policy_body "${name}" "${source}")"
        succeeded "update policy ${name}"
        echo "updated ${name}"
    fi
done

request GET /state
succeeded "read state"
if [ "$(jq -r .enabled "${RESPONSE}")" = true ]; then
    echo "unchanged state"
else
    request PUT /state '{"enabled":true}'
    succeeded "enable authorization"
    echo "enabled authorization"
fi
