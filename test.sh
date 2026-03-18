#!/usr/bin/env bash

set -euo pipefail

HIVE_HOST="${HIVE_HOST:-127.0.0.1}"
PROXY_PORT="${PROXY_PORT:-6666}"
MANAGEMENT_PORT="${MANAGEMENT_PORT:-6668}"
ADMIN_TOKEN="${ADMIN_TOKEN:-}"
WORKER_NAME="${WORKER_NAME:-}"
TEST_MODEL="${TEST_MODEL:-}"
PROMPT="${PROMPT:-Reply with exactly OK.}"
CLIENT_KEY_NAME_PREFIX="${CLIENT_KEY_NAME_PREFIX:-smoke-client}"
TIMEOUT_SECONDS="${TIMEOUT_SECONDS:-120}"
DEFAULT_MODEL="${DEFAULT_MODEL:-qwen3:0.6b}"

if [[ -z "${ADMIN_TOKEN}" ]]; then
    echo "ADMIN_TOKEN is required" >&2
    exit 1
fi

if [[ -z "${WORKER_NAME}" ]]; then
    echo "WORKER_NAME is required" >&2
    exit 1
fi

MANAGEMENT_BASE="http://${HIVE_HOST}:${MANAGEMENT_PORT}"
PROXY_BASE="http://${HIVE_HOST}:${PROXY_PORT}"

tmpdir="$(mktemp -d)"
trap 'rm -rf "${tmpdir}"' EXIT

step() {
    printf '\n==> %s\n' "$1"
}

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

extract_token() {
    sed -n 's/.*"token":"\([^"]*\)".*/\1/p' "$1" | head -n 1
}

extract_json_string_field() {
    local field="$1"
    local file="$2"
    sed -n "s/.*\"${field}\":\"\\([^\"]*\\)\".*/\\1/p" "${file}" | head -n 1
}

extract_first_worker_model() {
    local worker_name="$1"
    local file="$2"
    awk -v worker="${worker_name}" '
        index($0, "\"" worker "\":[") {
            match($0, "\"" worker "\":\\[[^]]*\\]")
            if (RSTART > 0) {
                entry = substr($0, RSTART, RLENGTH)
                sub("^\"" worker "\":\\[", "", entry)
                sub("\\]$", "", entry)
                if (entry == "") {
                    exit
                }
                split(entry, parts, ",")
                gsub(/^"/, "", parts[1])
                gsub(/"$/, "", parts[1])
                print parts[1]
                exit
            }
        }
    ' "${file}"
}

extract_distinct_worker_model() {
    local worker_name="$1"
    local exclude_model="$2"
    local file="$3"
    awk -v worker="${worker_name}" -v exclude="${exclude_model}" '
        index($0, "\"" worker "\":[") {
            match($0, "\"" worker "\":\\[[^]]*\\]")
            if (RSTART > 0) {
                entry = substr($0, RSTART, RLENGTH)
                sub("^\"" worker "\":\\[", "", entry)
                sub("\\]$", "", entry)
                if (entry == "") {
                    exit
                }
                n = split(entry, parts, ",")
                for (i = 1; i <= n; i++) {
                    gsub(/^"/, "", parts[i])
                    gsub(/"$/, "", parts[i])
                    if (parts[i] != "" && parts[i] != exclude) {
                        print parts[i]
                        exit
                    }
                }
            }
        }
    ' "${file}"
}

expect_status() {
    local actual="$1"
    local expected="$2"
    local context="$3"
    if [[ "${actual}" != "${expected}" ]]; then
        fail "${context}: expected HTTP ${expected}, got ${actual}"
    fi
}

contains_text() {
    local needle="$1"
    local file="$2"
    grep -Fq "${needle}" "${file}"
}

curl_json() {
    local method="$1"
    local url="$2"
    local body="${3:-}"
    local output_file="$4"

    if [[ -n "${body}" ]]; then
        curl -sS \
            --max-time "${TIMEOUT_SECONDS}" \
            -X "${method}" \
            -H "Content-Type: application/json" \
            -H "Authorization: Bearer ${ADMIN_TOKEN}" \
            -d "${body}" \
            -o "${output_file}" \
            -w "%{http_code}" \
            "${url}"
    else
        curl -sS \
            --max-time "${TIMEOUT_SECONDS}" \
            -X "${method}" \
            -H "Authorization: Bearer ${ADMIN_TOKEN}" \
            -o "${output_file}" \
            -w "%{http_code}" \
            "${url}"
    fi
}

create_key() {
    local name="$1"
    local role="$2"
    local whitelist_json="$3"
    local blacklist_json="$4"
    local output_file="$5"

    local body
    body="$(printf '{"name":"%s","role":"%s","whitelist_models":%s,"blacklist_models":%s}' \
        "${name}" "${role}" "${whitelist_json}" "${blacklist_json}")"

    local status
    status="$(curl_json POST "${MANAGEMENT_BASE}/key" "${body}" "${output_file}")"
    expect_status "${status}" "201" "create key ${name}"
}

client_generate() {
    local token="$1"
    local model="$2"
    local output_file="$3"

    local body
    body="$(printf '{"model":"%s","prompt":"%s","stream":false}' "${model}" "${PROMPT}")"

    curl -sS \
        --max-time "${TIMEOUT_SECONDS}" \
        -X POST \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer ${token}" \
        -d "${body}" \
        -o "${output_file}" \
        -w "%{http_code}" \
        "${PROXY_BASE}/api/generate"
}

client_json_request() {
    local token="$1"
    local method="$2"
    local path="$3"
    local body="$4"
    local output_file="$5"

    curl -sS \
        --max-time "${TIMEOUT_SECONDS}" \
        -X "${method}" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer ${token}" \
        -d "${body}" \
        -o "${output_file}" \
        -w "%{http_code}" \
        "${PROXY_BASE}${path}"
}

client_get_request() {
    local token="$1"
    local path="$2"
    local output_file="$3"

    curl -sS \
        --max-time "${TIMEOUT_SECONDS}" \
        -H "Authorization: Bearer ${token}" \
        -o "${output_file}" \
        -w "%{http_code}" \
        "${PROXY_BASE}${path}"
}

pull_model_to_worker() {
    local model="$1"
    local output_file="$2"
    local body
    body="$(printf '{"model":"%s","stream":false}' "${model}")"

    curl -sS \
        --max-time "${TIMEOUT_SECONDS}" \
        -X POST \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer ${ADMIN_TOKEN}" \
        -H "Node: ${WORKER_NAME}" \
        -d "${body}" \
        -o "${output_file}" \
        -w "%{http_code}" \
        "${PROXY_BASE}/api/pull"
}

step "Check admin access"
status="$(curl_json GET "${MANAGEMENT_BASE}/key" "" "${tmpdir}/keys.json")"
expect_status "${status}" "200" "management key listing"
contains_text '"role":"Admin"' "${tmpdir}/keys.json" || fail "admin key listing does not contain any Admin key"

step "Check connected worker"
status="$(curl_json GET "${MANAGEMENT_BASE}/worker/status" "" "${tmpdir}/worker_status.json")"
expect_status "${status}" "200" "worker status"
contains_text "\"${WORKER_NAME}\"" "${tmpdir}/worker_status.json" || fail "worker ${WORKER_NAME} not present in /worker/status"

status="$(curl_json GET "${MANAGEMENT_BASE}/worker/tags" "" "${tmpdir}/worker_tags.json")"
expect_status "${status}" "200" "worker tags"
contains_text "\"${WORKER_NAME}\"" "${tmpdir}/worker_tags.json" || fail "worker ${WORKER_NAME} not present in /worker/tags"

status="$(curl_json GET "${MANAGEMENT_BASE}/worker/versions" "" "${tmpdir}/worker_versions.json")"
expect_status "${status}" "200" "worker versions"
contains_text "\"${WORKER_NAME}\"" "${tmpdir}/worker_versions.json" || fail "worker ${WORKER_NAME} not present in /worker/versions"

step "Check queue endpoint"
status="$(curl_json GET "${MANAGEMENT_BASE}/queue" "" "${tmpdir}/queue.json")"
expect_status "${status}" "200" "queue listing"
contains_text '"model_queue"' "${tmpdir}/queue.json" || fail "queue response missing model_queue"
contains_text '"node_queue"' "${tmpdir}/queue.json" || fail "queue response missing node_queue"

step "Target the worker through the proxy"
status="$(
    curl -sS \
        --max-time "${TIMEOUT_SECONDS}" \
        -H "Authorization: Bearer ${ADMIN_TOKEN}" \
        -H "Node: ${WORKER_NAME}" \
        -o "${tmpdir}/targeted_tags.json" \
        -w "%{http_code}" \
        "${PROXY_BASE}/api/tags"
)"
expect_status "${status}" "200" "targeted proxy request to /api/tags"
contains_text '"models"' "${tmpdir}/targeted_tags.json" || fail "targeted /api/tags response does not look like Ollama"

step "Resolve test model from management data"
resolved_model="$(extract_first_worker_model "${WORKER_NAME}" "${tmpdir}/worker_tags.json" || true)"

if [[ -z "${resolved_model}" ]]; then
    echo "Worker ${WORKER_NAME} has no advertised models. Pulling ${DEFAULT_MODEL}."
    status="$(pull_model_to_worker "${DEFAULT_MODEL}" "${tmpdir}/pull_model.json")"
    expect_status "${status}" "200" "pull default model to worker"
    contains_text '"success"' "${tmpdir}/pull_model.json" || contains_text '"completed"' "${tmpdir}/pull_model.json" || contains_text '"status"' "${tmpdir}/pull_model.json" || fail "pull response did not contain an expected progress/status field"
    resolved_model="${DEFAULT_MODEL}"
fi

if [[ -n "${TEST_MODEL}" && "${TEST_MODEL}" != "${resolved_model}" ]]; then
    echo "TEST_MODEL=${TEST_MODEL} overrides auto-selected model ${resolved_model}."
    resolved_model="${TEST_MODEL}"
fi

echo "Using test model: ${resolved_model}"
hidden_model="$(extract_distinct_worker_model "${WORKER_NAME}" "${resolved_model}" "${tmpdir}/worker_tags.json" || true)"

if [[ -n "${resolved_model}" ]]; then
    step "Create whitelist-limited client key"
    allowed_name="${CLIENT_KEY_NAME_PREFIX}-allowed-$(date +%s)"
    create_key "${allowed_name}" "Client" "[\"${resolved_model}\"]" "[]" "${tmpdir}/allowed_key.json"
    allowed_token="$(extract_token "${tmpdir}/allowed_key.json")"
    [[ -n "${allowed_token}" ]] || fail "failed to extract allowed client token"

    step "Run allowed client request"
    status="$(client_generate "${allowed_token}" "${resolved_model}" "${tmpdir}/allowed_generate.json")"
    expect_status "${status}" "200" "allowed generate request"
    contains_text '"response"' "${tmpdir}/allowed_generate.json" || contains_text '"done"' "${tmpdir}/allowed_generate.json" || fail "generate response missing expected Ollama fields"

    step "Verify model masking on discovery routes"
    status="$(client_get_request "${allowed_token}" "/api/tags" "${tmpdir}/allowed_tags.json")"
    expect_status "${status}" "200" "allowed key /api/tags request"
    contains_text "\"${resolved_model}\"" "${tmpdir}/allowed_tags.json" || fail "allowed key cannot see its whitelisted model in /api/tags"
    if [[ -n "${hidden_model}" ]]; then
        if contains_text "\"${hidden_model}\"" "${tmpdir}/allowed_tags.json"; then
            fail "allowed key can see hidden model ${hidden_model} in /api/tags"
        fi
    fi

    status="$(client_get_request "${allowed_token}" "/api/ps" "${tmpdir}/allowed_ps.json")"
    expect_status "${status}" "200" "allowed key /api/ps request"
    if [[ -n "${hidden_model}" ]]; then
        if contains_text "\"${hidden_model}\"" "${tmpdir}/allowed_ps.json"; then
            fail "allowed key can see hidden model ${hidden_model} in /api/ps"
        fi
    fi

    if [[ -n "${hidden_model}" ]]; then
        step "Verify hidden model access is blocked"
        status="$(client_json_request "${allowed_token}" "POST" "/api/show" "{\"model\":\"${hidden_model}\"}" "${tmpdir}/hidden_show.json")"
        expect_status "${status}" "403" "hidden model show request"
    fi

    step "Create blacklist-limited client key"
    denied_name="${CLIENT_KEY_NAME_PREFIX}-denied-$(date +%s)"
    create_key "${denied_name}" "Client" "[]" "[\"${resolved_model}\"]" "${tmpdir}/denied_key.json"
    denied_token="$(extract_token "${tmpdir}/denied_key.json")"
    [[ -n "${denied_token}" ]] || fail "failed to extract denied client token"

    step "Confirm blacklist enforcement"
    status="$(client_generate "${denied_token}" "${resolved_model}" "${tmpdir}/denied_generate.json")"
    expect_status "${status}" "403" "blacklisted generate request"
else
    step "Skip model generate checks"
    echo "No model was resolved for testing."
fi

step "Smoke test completed"
echo "Worker ${WORKER_NAME} is reachable through management and proxy endpoints."
