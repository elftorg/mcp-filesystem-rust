#!/usr/bin/env bash
set -euo pipefail

MCP_URL="${MCP_URL:-https://b24.alwaysdata.net/mcpfs/mcp}"
HEALTH_URL="${HEALTH_URL:-https://b24.alwaysdata.net/mcpfs/health}"
BACKEND_HEALTH_URL="${BACKEND_HEALTH_URL:-https://b24.alwaysdata.net/mcpfs/health/backend}"
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT
cd "$WORK_DIR"

request_json() {
  local label="$1" url="$2" output="$3"
  echo "[$label] GET $url"
  local status
  status="$(curl --silent --show-error --connect-timeout 10 --max-time 20 -o "$output" -w '%{http_code}' "$url")"
  echo "[$label] HTTP $status: $(tr -d '\n' < "$output" | head -c 500)"
  test "$status" = 200
}

request_json proxy-health "$HEALTH_URL" health.json
jq -e '.status == "ok"' health.json >/dev/null
request_json backend-health "$BACKEND_HEALTH_URL" backend-health.json
jq -e '.status == "UP" or .status == "ok"' backend-health.json >/dev/null

echo '[initialize] POST MCP initialize'
INIT_STATUS="$(curl --silent --show-error --connect-timeout 10 --max-time 30 \
  -D initialize.headers -o initialize.json -w '%{http_code}' \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"github-actions-endpoint-test","version":"1.0"}}}' \
  "$MCP_URL")"
echo "[initialize] HTTP $INIT_STATUS: $(tr -d '\n' < initialize.json | head -c 1000)"
test "$INIT_STATUS" = 200
SESSION_ID="$(awk 'BEGIN { IGNORECASE=1 } /^MCP-Session-Id:/ { sub(/^[^:]+:[[:space:]]*/, ""); sub(/\r$/, ""); print; exit }' initialize.headers)"
test -n "$SESSION_ID"
jq -e '.jsonrpc == "2.0" and .result.protocolVersion != null and .result.capabilities.tools != null' initialize.json >/dev/null
echo "[initialize] session ${SESSION_ID:0:8}..."

NOTIFY_STATUS="$(curl --silent --show-error --connect-timeout 10 --max-time 20 \
  -o initialized.body -w '%{http_code}' \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -H "MCP-Session-Id: $SESSION_ID" \
  -H 'MCP-Protocol-Version: 2025-11-25' \
  --data '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  "$MCP_URL")"
echo "[initialized] HTTP $NOTIFY_STATUS: $(tr -d '\n' < initialized.body | head -c 500)"
case "$NOTIFY_STATUS" in 200|202|204) ;; *) exit 1;; esac

rpc_call() {
  local id="$1" method="$2" output="$3"
  local status
  status="$(curl --silent --show-error --connect-timeout 10 --max-time 30 \
    -o "$output" -w '%{http_code}' \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -H "MCP-Session-Id: $SESSION_ID" \
    -H 'MCP-Protocol-Version: 2025-11-25' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":$id,\"method\":\"$method\"}" \
    "$MCP_URL")"
  echo "[$method] HTTP $status: $(tr -d '\n' < "$output" | head -c 1000)"
  test "$status" = 200
}

rpc_call 2 tools/list tools.json
TOOL_COUNT="$(jq '.result.tools | length' tools.json)"
echo "[tools/list] tools=$TOOL_COUNT names=$(jq -c '.result.tools | map(.name)' tools.json)"
test "$TOOL_COUNT" -gt 0

rpc_call 3 resources/list resources.json
RESOURCE_COUNT="$(jq '.result.resources | length' resources.json)"
echo "[resources/list] resources=$RESOURCE_COUNT uris=$(jq -c '.result.resources | map(.uri) | .[0:10]' resources.json)"
test "$RESOURCE_COUNT" -gt 0
jq -e '.result.resources | any(.uri == "ui://filesystem/browser-v1.html")' resources.json >/dev/null

set +e
SSE_STATUS="$(curl --silent --show-error --no-buffer --connect-timeout 10 --max-time 5 \
  -D sse.headers -o sse.body -w '%{http_code}' \
  -H 'Accept: text/event-stream' \
  -H "MCP-Session-Id: $SESSION_ID" \
  -H 'MCP-Protocol-Version: 2025-11-25' \
  "$MCP_URL")"
SSE_CURL_STATUS=$?
set -e
echo "[SSE] HTTP $SSE_STATUS curl=$SSE_CURL_STATUS content-type=$(awk 'BEGIN{IGNORECASE=1}/^content-type:/{sub(/\r$/,"");print $0;exit}' sse.headers) body=$(tr -d '\n' < sse.body | head -c 500)"
test "$SSE_STATUS" = 200
grep -qi '^content-type: text/event-stream' sse.headers
test "$SSE_CURL_STATUS" -eq 0 -o "$SSE_CURL_STATUS" -eq 28

DELETE_STATUS="$(curl --silent --show-error --connect-timeout 10 --max-time 20 \
  -o delete.body -w '%{http_code}' -X DELETE \
  -H "MCP-Session-Id: $SESSION_ID" \
  -H 'MCP-Protocol-Version: 2025-11-25' \
  "$MCP_URL")"
echo "[DELETE] HTTP $DELETE_STATUS: $(tr -d '\n' < delete.body | head -c 500)"
test "$DELETE_STATUS" = 204

echo '[result] All public MCP endpoint checks passed.'
