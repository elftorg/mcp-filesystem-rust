# MCP Filesystem Server (Rust)

A high-performance, sandboxed Model Context Protocol server for filesystem
operations. It supports stdio, legacy JSON-RPC HTTP, MCP Streamable HTTP,
filesystem Resources, and an MCP Apps UI for ChatGPT.

## Status

This repository exposes 41 tools across opt-in categories and keeps the
original MCP contracts intact. The `codex/chatgpt-apps-sdk` branch adds the
MVP-1 Apps layer:

- `POST /mcp` and `GET /mcp`;
- JSON and Server-Sent Events;
- `MCP-Session-Id` lifecycle;
- `resources/list`, `resources/read`, `resources/templates/list`;
- `file://` resources with MIME, size, preview and modification time;
- `ui://filesystem/browser-v1.html`;
- interactive file browser and preview component;
- Docker/Compose deployment templates.

OAuth, per-user sandbox roots and audit logging are specified for MVP-2 in
[`docs/oauth.md`](docs/oauth.md), but are not claimed as implemented.

## Features

- Rust stable, Tokio async runtime and Axum.
- MCP JSON-RPC 2.0 with protocol negotiation through `2025-11-25`.
- stdio transport for desktop/local MCP hosts.
- Streamable HTTP transport at `/mcp`.
- Backward-compatible `POST /rpc`.
- `structuredContent`, typed media content and tool annotations.
- MCP Resources for files and the Apps UI component.
- Capability-backed sandbox built on `cap_std`.
- Canonical allow-list checks and symlink rejection by default.
- Read-only policy and explicit tool category gates.
- Request, file and decompression size limits.
- Optional static bearer token and in-process rustls TLS.
- IPv4 and IPv6 bind support.
- Health and diagnostics endpoints.

## Quick start

```bash
cargo build --release
./target/release/mcp-filesystem \
  --directories /srv/files \
  --enable-read
```

The HTTP server listens on `127.0.0.1:3001` by default.

### stdio

```bash
mcp-filesystem \
  --stdio \
  --directories /srv/files \
  --enable-read
```

### Private HTTP deployment

```bash
mcp-filesystem \
  --directories /srv/files \
  --host 0.0.0.0 \
  --http-port 3001 \
  --auth-token "$MCP_AUTH_TOKEN" \
  --access-mode readonly \
  --enable-read
```

Use HTTPS directly or terminate TLS at a trusted reverse proxy before
exposing the server remotely.

## Tool categories

No tools are advertised unless at least one category is enabled.

| Flag | Category |
|---|---|
| `--enable-read` | read, list, search, stat and hash |
| `--enable-write` | create, write, edit, copy and move |
| `--enable-delete` | file and directory deletion |
| `--enable-compress` | gzip, zstd and tar |
| `--enable-crypto` | encryption, decryption and key generation |
| `--enable-csv` | CSV read and mutation tools |
| `--enable-all` | every category |

The complete tool schemas are defined in [`tools.json`](tools.json).

`--access-mode readonly` remains an additional runtime restriction. OAuth
scopes planned for MVP-2 will only reduce access further; they will never
activate a disabled category.

## MCP Streamable HTTP

The canonical remote MCP endpoint is:

```text
https://your-domain.example/mcp
```

### Initialize

```bash
curl -i http://127.0.0.1:3001/mcp \
  -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' \
  -d '{
    "jsonrpc":"2.0",
    "id":1,
    "method":"initialize",
    "params":{
      "protocolVersion":"2025-11-25",
      "capabilities":{},
      "clientInfo":{"name":"example","version":"1.0"}
    }
  }'
```

The response contains `MCP-Session-Id`. Include it on all subsequent `/mcp`
requests:

```bash
curl http://127.0.0.1:3001/mcp \
  -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' \
  -H 'mcp-session-id: <session-id>' \
  -H 'mcp-protocol-version: 2025-11-25' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
```

Open the server SSE stream with `GET /mcp` and
`Accept: text/event-stream`. A session can be terminated with `DELETE /mcp`.

### Legacy endpoint

Existing clients may continue to use:

```text
POST /rpc
Content-Type: application/json
```

The legacy endpoint is intentionally stateless and does not require
`MCP-Session-Id`.

## Filesystem Resources

The server advertises the MCP `resources` capability.

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "resources/list"
}
```

Regular files under every allowed directory are exposed as `file://` URIs.
`resources/read` returns text for textual formats and base64 `blob` for binary
formats. URI decoding is followed by the same capability sandbox validation
used by tools.

The resource list is paginated, excludes hidden path components and is
bounded to prevent unbounded scans.

## ChatGPT / MCP Apps UI

Browser-oriented tools reference:

```text
ui://filesystem/browser-v1.html
```

The resource MIME type is:

```text
text/html;profile=mcp-app
```

The component provides:

- allowed directory selection;
- directory and file lists;
- glob search;
- text, Markdown and JSON preview;
- CSV table preview;
- image preview;
- responsive light/dark layout.

The UI uses the open MCP Apps `postMessage` bridge first and treats
`window.openai` as an optional compatibility enhancement.

Tool descriptors use current metadata:

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

See [`docs/chatgpt-apps.md`](docs/chatgpt-apps.md) for setup and protocol
examples.

## Security model

Every filesystem path must resolve below an explicitly allowed root.

Protection layers include:

- canonical path containment;
- capability-relative filesystem handles;
- symlink components denied by default;
- opt-in write/delete/crypto/compression categories;
- read-only mode;
- file, request and decompression limits;
- constant-time static token comparison;
- same-origin or `MCP_ALLOWED_ORIGINS` validation;
- session and MCP protocol-version validation;
- hidden files omitted from advertised resources.

Never mount `/`, a home directory containing secrets, Docker socket paths or
system configuration directories into a remote deployment.

### Authentication status

MVP-1 supports an optional static bearer token for private deployments. A
public multi-user ChatGPT app requires OAuth authorization code + PKCE,
protected-resource metadata, audience validation, scopes, refresh token
rotation and per-user storage roots. The required design is documented in
[`docs/oauth.md`](docs/oauth.md).

## HTTP endpoints

| Method | Path | Purpose |
|---|---|---|
| `POST` | `/mcp` | Streamable HTTP JSON-RPC |
| `GET` | `/mcp` | session SSE stream |
| `DELETE` | `/mcp` | close session |
| `POST` | `/rpc` | legacy JSON-RPC |
| `GET` | `/health` | health check |
| `GET` | `/version` | version |
| `GET` | `/info` | runtime configuration summary |
| `GET` | `/tools` | filtered tool descriptors |

## Important options

| Option | Default | Description |
|---|---:|---|
| `--directories <PATH>` | current directory | allowed root; repeatable |
| `--host <HOST>` | `127.0.0.1` | HTTP bind host |
| `--http-port <PORT>` | `3001` | HTTP port |
| `--stdio` | false | use stdio instead of HTTP |
| `--access-mode <MODE>` | unrestricted | `unrestricted` or `readonly` |
| `--follow-symlinks` | false | allow symlink traversal |
| `--auth-token <TOKEN>` | none | static HTTP bearer token |
| `--tls-cert <PATH>` | none | PEM certificate chain |
| `--tls-key <PATH>` | none | PEM private key |
| `--max-file-size <MB>` | 100 | maximum read size |
| `--max-decompressed-size <MB>` | 1024 | extraction output limit |
| `--max-request-bytes <BYTES>` | 16777216 | stdio request limit |
| `--max-http-body-bytes <BYTES>` | 16777216 | HTTP request limit |
| `--request-timeout <SECONDS>` | 30 | per-request timeout |

`MCP_ALLOWED_ORIGINS` is a comma-separated environment variable used for
non-same-origin browser requests.

## Docker

```bash
cp .env.example .env
docker compose up -d --build
```

The default Compose profile:

- runs as an unprivileged user;
- drops Linux capabilities;
- uses a read-only root filesystem;
- mounts `/data` read-only;
- enables only read tools;
- requires a static bearer token;
- checks `/health`.

See [`docs/deployment.md`](docs/deployment.md) for reverse proxy and scaling
requirements.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features
cargo test --all-features
cargo build --release
```

CI runs the same checks on Rust stable.

## Documentation

- [Current-code audit](docs/chatgpt-apps-audit.md)
- [ChatGPT Apps setup](docs/chatgpt-apps.md)
- [OAuth and multi-user design](docs/oauth.md)
- [Deployment](docs/deployment.md)
- [Migration notes](MIGRATION.md)
- [Changelog](CHANGELOG.md)

## MVP roadmap

### MVP-1 — implemented in this branch

- Streamable HTTP;
- Resources API;
- Apps UI resource;
- File Browser;
- deployment foundation.

### MVP-2

- OAuth and discovery metadata;
- scope enforcement;
- request-scoped user sandbox;
- rotating refresh tokens;
- audit sink and JSONL output;
- shared session store.

### MVP-3

- enterprise policy engine;
- quotas and rate limits;
- distributed SSE notifications;
- admin console;
- advanced previews and upload workflows.

## License

Apache-2.0.
