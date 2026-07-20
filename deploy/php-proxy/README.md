# PHP proxy for MCP Streamable HTTP

This directory contains a shared-hosting reverse proxy for
`mcp-filesystem-rust`. It preserves MCP session and protocol headers, forwards
Bearer authorization, streams Server-Sent Events without application-level
buffering, strips the public path prefix, and exposes proxy/backend health
checks.

## Requirements

- PHP 8.1 or newer;
- PHP cURL extension;
- Apache with `mod_rewrite` for the included `.htaccess`;
- a public HTTPS website;
- a reachable `mcp-filesystem-rust` HTTP backend.

A long-lived `GET /mcp` request must be allowed by the hosting platform. Some
shared hosts impose a hard PHP execution limit that cannot be disabled from
application code; in that case use Nginx, Caddy, Cloudflare Tunnel, or the Rust
server's built-in TLS instead.

## Install

Copy this directory to the public website, for example as `/mcpfs`:

```text
/public_html/mcpfs/
├── .htaccess
├── index.php
├── config.php
└── .gitignore
```

Create the local configuration:

```bash
cp config.php.example config.php
```

Set `backend_url` in `config.php`, or configure environment variables:

| Variable | Default | Purpose |
|---|---|---|
| `MCP_BACKEND_URL` | `http://services-b24.alwaysdata.net:8345` | Rust MCP origin |
| `MCP_PROXY_BASE_PATH` | `/mcpfs` | Public URL prefix stripped before forwarding |
| `MCP_PROXY_CONNECT_TIMEOUT` | `5` | Upstream connect timeout in seconds |
| `MCP_PROXY_REQUEST_TIMEOUT` | `60` | Non-SSE request timeout; `0` disables it |
| `MCP_PROXY_MAX_REQUEST_BYTES` | `16777216` | Maximum request body |
| `MCP_PROXY_ALLOWED_PATHS` | MCP and diagnostic paths | Comma-separated exact paths |
| `MCP_PROXY_FORWARD_ORIGINAL_HOST` | `false` | Forward the public `Host` header |

The default public MCP endpoint is:

```text
https://your-domain.example/mcpfs/mcp
```

The proxy removes `/mcpfs`, so the backend receives `/mcp`.

## Backend command

Example read-only backend:

```bash
mcp-filesystem \
  --directories /srv/files \
  --host 0.0.0.0 \
  --http-port 8345 \
  --access-mode readonly \
  --enable-read
```

When requests include a browser `Origin`, configure the Rust process with the
public origin:

```bash
export MCP_ALLOWED_ORIGINS="https://your-domain.example,https://chatgpt.com"
```

When the backend uses `--auth-token`, configure the ChatGPT connector/client to
send the same `Authorization: Bearer ...` header. The PHP proxy forwards it and
does not store the token.

## Checks

Proxy health:

```bash
curl -fsS https://your-domain.example/mcpfs/health
```

Backend health through the proxy:

```bash
curl -fsS https://your-domain.example/mcpfs/health/backend
```

Initialize MCP and inspect the returned `MCP-Session-Id` header:

```bash
curl -i https://your-domain.example/mcpfs/mcp \
  -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' \
  -d '{
    "jsonrpc":"2.0",
    "id":1,
    "method":"initialize",
    "params":{
      "protocolVersion":"2025-11-25",
      "capabilities":{},
      "clientInfo":{"name":"curl","version":"1.0"}
    }
  }'
```

Use the returned session ID on subsequent calls:

```bash
curl https://your-domain.example/mcpfs/mcp \
  -H 'content-type: application/json' \
  -H 'accept: application/json, text/event-stream' \
  -H 'mcp-session-id: <session-id>' \
  -H 'mcp-protocol-version: 2025-11-25' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
```

## Security

- expose only dedicated sandbox directories from the Rust process;
- prefer `--access-mode readonly` and `--enable-read` initially;
- remove `/info` and `/tools` from `allowed_paths` when they should be private;
- never commit `config.php` when it contains deployment-specific values;
- use OAuth before distributing a multi-user ChatGPT app.
