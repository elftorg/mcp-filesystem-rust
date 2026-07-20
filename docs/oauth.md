# OAuth 2.0 and multi-user design

## Status

OAuth endpoints, per-user sandboxes and audit logging are **MVP-2** and are
not enabled by this branch. The existing optional static bearer token remains
available for private deployments.

This is intentional: a production ChatGPT authorization flow must not be
implemented as an in-memory username/password demo.

## Required protocol surface

The production resource server must expose:

```text
GET /.well-known/oauth-protected-resource
```

Example:

```json
{
  "resource": "https://files.example.com",
  "authorization_servers": ["https://auth.example.com"],
  "scopes_supported": [
    "filesystem.read",
    "filesystem.write",
    "filesystem.admin"
  ],
  "resource_documentation": "https://files.example.com/docs/oauth"
}
```

The authorization server must publish OAuth 2.0 or OIDC metadata, including:

- `authorization_endpoint`;
- `token_endpoint`;
- PKCE `S256`;
- supported token endpoint auth methods;
- CIMD, DCR, or a preconfigured ChatGPT client;
- optional revocation/introspection endpoints.

The authorization and token endpoints shown in `app/manifest.json` are target
URLs, not a claim that the MVP server currently implements them.

## Flow

1. ChatGPT discovers protected-resource metadata.
2. ChatGPT identifies/registers its OAuth client.
3. `/authorize` validates `client_id`, `redirect_uri`, `resource`, scopes,
   `state`, `code_challenge` and `code_challenge_method=S256`.
4. The user authenticates with the selected identity provider and grants
   consent.
5. A short-lived, single-use authorization code is stored server-side.
6. `/token` validates the PKCE verifier and issues:
   - short-lived access token;
   - rotating refresh token;
   - subject/user identifier;
   - `aud` bound to the MCP resource;
   - granted scopes.
7. Every `/mcp` request validates signature, expiry, issuer, audience and
   scopes.
8. Invalid or insufficient tokens produce `401`/`403`, a
   `WWW-Authenticate` challenge and, for tool results, MCP auth metadata.

## Scope policy

Suggested mapping:

| Scope | Allowed operations |
|---|---|
| `filesystem.read` | list, read, search, stat, hash, resources |
| `filesystem.write` | create, write, edit, copy, move, CSV mutations |
| `filesystem.admin` | delete, permissions, symlink, compression, crypto |

Tool category flags remain an operator-side upper bound. OAuth scopes can only
reduce access; they cannot activate a disabled category.

## Per-tool metadata

MVP-2 should add `securitySchemes` to every tool descriptor. Example:

```json
{
  "securitySchemes": [
    {
      "type": "oauth2",
      "scopes": ["filesystem.read"]
    }
  ]
}
```

A write tool must request `filesystem.write`; delete/permission/crypto tools
must request `filesystem.admin`.

## Multi-user sandbox

Recommended model:

```text
/storage/users/{stable_subject}/
```

`stable_subject` must be derived from the validated token issuer + subject,
not from a client-supplied header or path.

Request context:

```text
AuthenticatedPrincipal
  subject
  issuer
  scopes
  sandbox_root
  token_id
```

Create a request-scoped `Config`/`Sandbox` whose allow-list is the user's
canonical root. Never mutate a global allow-list between concurrent requests.

Administrative shared roots, if required, should be explicit grants stored in
a database:

```text
user_id -> canonical root -> read/write/admin policy
```

## Token storage

Do not persist plaintext refresh tokens. Store a keyed hash, token family,
expiry, user, client and revocation state. Rotate on every refresh and revoke
the complete family on reuse detection.

Signing keys require:

- KMS/HSM or restricted secret storage;
- key IDs;
- overlap during rotation;
- published JWKS for asymmetric tokens;
- audit events for issuance, refresh and revocation.

## Audit logging

Target JSONL record:

```json
{
  "timestamp": "2026-07-20T12:00:00Z",
  "request_id": "uuid",
  "session_id": "uuid",
  "user": "issuer|subject",
  "scopes": ["filesystem.read"],
  "tool": "read_text_file",
  "operation": "read",
  "path": "/docs/test.md",
  "result": "success",
  "duration_ms": 12
}
```

Rules:

- never log file contents, tokens, encryption keys or authorization codes;
- log canonical user-relative paths where possible;
- rotate by size/date;
- restrict filesystem permissions;
- support stdout JSON for container log collection;
- define retention and deletion policies;
- add integrity protection for regulated environments.

## Recommended implementation sequence

1. Introduce `RequestContext` without changing behavior.
2. Move tool dispatch and resources to context-scoped sandbox access.
3. Integrate an external OIDC/OAuth provider or a proven authorization-server
   crate/service.
4. Add protected-resource metadata and challenges.
5. Add scope mapping and negative authorization tests.
6. Add per-user storage roots and migration tooling.
7. Add audit sink abstraction and JSONL implementation.
8. Add Redis/database-backed sessions, authorization codes and token state.
9. Validate with MCP Inspector and ChatGPT development connector.
