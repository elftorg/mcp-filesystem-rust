# Deployment

## Docker

Build:

```bash
docker build -t mcp-filesystem-app .
```

Run a read-only tool surface:

```bash
docker run --rm \
  -p 3001:3001 \
  -v /srv/files:/data:ro \
  mcp-filesystem-app \
  --directories /data \
  --host 0.0.0.0 \
  --http-port 3001 \
  --access-mode readonly \
  --enable-read
```

For a writable sandbox, mount a dedicated volume and explicitly enable only
the required categories.

## Docker Compose

```bash
cp .env.example .env
# Set a long random MCP_AUTH_TOKEN and deployment paths.
docker compose up -d --build
```

The Compose template exposes port 3001, mounts a single data directory and
requires a static bearer token. This is suitable for private MVP testing, not
multi-user ChatGPT distribution.

## Reverse proxy

Terminate public TLS at a reverse proxy and forward to the container:

```text
https://domain.example/mcp -> http://127.0.0.1:3001/mcp
```

Requirements:

- HTTP/1.1 or HTTP/2 without response buffering for SSE;
- long read timeout for `GET /mcp`;
- preserve `Authorization`, `Accept`, `Content-Type`,
  `MCP-Session-Id`, `MCP-Protocol-Version`, `Origin`, and `Host`;
- disable proxy caching for `/mcp`;
- do not compress or buffer SSE aggressively;
- limit request body size consistently with `--max-http-body-bytes`.

Example Nginx location:

```nginx
location /mcp {
    proxy_pass http://127.0.0.1:3001;
    proxy_http_version 1.1;
    proxy_set_header Host $host;
    proxy_set_header Authorization $http_authorization;
    proxy_set_header Origin $http_origin;
    proxy_set_header MCP-Session-Id $http_mcp_session_id;
    proxy_set_header MCP-Protocol-Version $http_mcp_protocol_version;
    proxy_buffering off;
    proxy_read_timeout 1h;
    client_max_body_size 16m;
}
```

Also proxy `/health` for orchestration checks. Keep `/info` and `/tools`
private if their operational details should not be public.

### PHP proxy for shared hosting

For Apache/PHP hosting where Nginx or Caddy configuration is unavailable, use
the proxy in [`deploy/php-proxy`](../deploy/php-proxy/README.md). It supports:

- public path prefixes such as `/mcpfs/mcp` with forwarding to backend `/mcp`;
- `POST`, `GET`/SSE and `DELETE` MCP requests;
- `MCP-Session-Id`, `MCP-Protocol-Version`, `Authorization` and `Origin` headers;
- streamed backend response bodies and response headers;
- proxy and backend health endpoints;
- environment variables or a local ignored `config.php`.

PHP hosting must allow long-running requests for the `GET /mcp` SSE stream.
Use a native reverse proxy when the provider enforces a short PHP execution
limit or buffers FastCGI output.

## Environment

| Variable | Purpose |
|---|---|
| `MCP_ALLOWED_ORIGINS` | comma-separated non-same-origin browser origins |
| `MCP_TLS_CERT` | optional PEM chain when terminating TLS in the process |
| `MCP_TLS_KEY` | optional PEM key |
| `RUST_LOG` | tracing filter |
| `MCP_AUTH_TOKEN` | used by the Compose command as `--auth-token` |

## Health

```bash
curl -fsS https://domain.example/health
```

Expected fields include `status`, `version`, `transport`, `mcpEndpoint` and
`legacyEndpoint`.

## Production hardening

- use a dedicated unprivileged UID;
- mount only user/application data, never host root paths;
- default to read-only mounts and `--access-mode readonly`;
- leave `--follow-symlinks` disabled;
- enable only required tool categories;
- put rate limits at the edge;
- set CPU, memory, PID and file descriptor limits;
- use a read-only container root filesystem plus a dedicated writable volume;
- forward structured logs to centralized storage;
- monitor active sessions and request latency;
- use OAuth before multi-user ChatGPT distribution;
- move session state to Redis/database before horizontal scaling.

## Scaling limitation

MVP-1 session IDs are stored in process memory. Use one replica or sticky
routing. Horizontal scaling requires a shared session store and an SSE
message broker.
