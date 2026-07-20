# ChatGPT Apps / OpenAI Apps SDK

## Scope

The branch `codex/chatgpt-apps-sdk` implements MVP-1:

- MCP Streamable HTTP at `POST /mcp` and `GET /mcp`;
- JSON and SSE response negotiation;
- `MCP-Session-Id` session lifecycle;
- filesystem Resources API;
- MCP Apps UI resource and interactive file browser;
- Apps SDK metadata on browser-oriented tools;
- legacy `POST /rpc` and stdio compatibility.

OAuth, per-user roots and audit logging are designed in `oauth.md` and are
not presented as complete in MVP-1.

## Start the server

```bash
cargo run --release -- \
  --directories /srv/files \
  --host 127.0.0.1 \
  --http-port 3001 \
  --enable-read
```

For a private remote deployment, use HTTPS and a bearer token:

```bash
cargo run --release -- \
  --directories /srv/files \
  --host 0.0.0.0 \
  --http-port 3001 \
  --auth-token "$MCP_AUTH_TOKEN" \
  --enable-read
```

Set explicit browser origins when requests may contain an `Origin` header:

```bash
export MCP_ALLOWED_ORIGINS="https://chatgpt.com,https://example.com"
```

A request without `Origin` is accepted for server-to-server MCP clients.
An origin is accepted when it matches `Host` or appears in
`MCP_ALLOWED_ORIGINS`. `Origin: null` is rejected.

## Streamable HTTP

### Initialize

```bash
curl -i https://example.com/mcp \
  -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
      "protocolVersion": "2025-11-25",
      "capabilities": {},
      "clientInfo": {"name": "example", "version": "1.0"}
    }
  }'
```

The response includes:

```text
MCP-Session-Id: <uuid>
```

Send this header on every subsequent `/mcp` request.

### List tools

```bash
curl https://example.com/mcp \
  -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' \
  -H 'mcp-session-id: <uuid>' \
  -H 'mcp-protocol-version: 2025-11-25' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
```

### Open the server SSE stream

```bash
curl -N https://example.com/mcp \
  -H 'accept: text/event-stream' \
  -H 'mcp-session-id: <uuid>' \
  -H 'mcp-protocol-version: 2025-11-25'
```

The stream uses SSE keep-alives. The current MVP has no server-originated
notifications queue; this endpoint establishes a spec-compatible stream and
is the extension point for progress, resource-change and logging events.

### Close a session

The implementation also accepts `DELETE /mcp` with `MCP-Session-Id` and
returns `204 No Content`.

## Resources

The server advertises:

```json
{
  "capabilities": {
    "resources": {
      "subscribe": false,
      "listChanged": false
    }
  }
}
```

Implemented methods:

- `resources/list` with an opaque numeric cursor and pages of 250;
- `resources/read`;
- `resources/templates/list`.

`resources/list` contains:

- `ui://filesystem/browser-v1.html`;
- regular files below every allowed root as `file://` URIs.

Each file descriptor includes MIME type, byte size, text preview and
modification time. Listing is capped at 10,000 files per scan and excludes
hidden path components.

Example:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "resources/read",
  "params": {
    "uri": "file:///srv/files/docs/readme.md"
  }
}
```

Text resources return `text`; binary resources return base64 `blob`.
All `file://` URIs are decoded and revalidated through the existing
capability-backed sandbox.

## UI resource

The component URI is:

```text
ui://filesystem/browser-v1.html
```

It is returned with:

```text
text/html;profile=mcp-app
```

At build time the Rust resource handler embeds `index.html`, `style.css` and
`app.js` into one self-contained HTML document.

The UI supports:

- allowed-root selection;
- directory listing;
- glob search;
- text/Markdown/JSON preview;
- CSV table preview;
- image preview;
- responsive light/dark rendering.

The primary bridge is standard JSON-RPC over `postMessage` using
`tools/call` and `ui/notifications/tool-result`. `window.openai.callTool`,
`window.openai.toolOutput` and `openai:set_globals` are optional
compatibility enhancements.

## Tool metadata

Browser-oriented tools contain:

```json
{
  "_meta": {
    "ui": {
      "resourceUri": "ui://filesystem/browser-v1.html",
      "visibility": ["model", "app"]
    },
    "openai/outputTemplate": "ui://filesystem/browser-v1.html"
  }
}
```

The compatibility alias is emitted alongside the current MCP Apps field.
Tool results also include the resource URI in `_meta`.

## ChatGPT setup

1. Deploy the server on a public HTTPS origin.
2. Verify `GET /health`.
3. Verify initialization and `tools/list` with an MCP Inspector.
4. Add the remote MCP endpoint `https://your-domain.example/mcp` in the
   ChatGPT app/connector development flow.
5. For MVP testing, run without auth on a tightly restricted test endpoint or
   use the static bearer mechanism supported by your client/reverse proxy.
6. Before user distribution, implement the OAuth design in `oauth.md`.

`app/manifest.json` is repository deployment metadata. MCP tool/resource
discovery remains the authoritative integration contract.

## Backward compatibility

Unchanged:

- stdio transport;
- all existing tool names and input schemas;
- opt-in tool category flags;
- access mode;
- sandbox rules;
- TLS flags;
- `POST /rpc`;
- diagnostic endpoints.

New `/mcp` session requirements do not apply to `/rpc`.
