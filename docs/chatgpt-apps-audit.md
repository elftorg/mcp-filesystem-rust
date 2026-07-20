# Аудит сумісності з ChatGPT Apps SDK

Дата аудиту: 20.07.2026  
Базова гілка: `main`  
Базовий commit: `603f4f0f65ba127eac9d8770890bc47a90db424a`

## Висновок

Проєкт уже мав зріле MCP-ядро: JSON-RPC 2.0, stdio, HTTP `POST /rpc`,
41 opt-in tool, `structuredContent`, TLS, статичний bearer token і
capability-backed filesystem sandbox. Для ChatGPT Apps бракувало саме
App Server шару: Streamable HTTP, реальних Resources та UI resource.

Архітектуру можна розширити без заміни на інший MCP SDK. Поточний
dispatcher, tool registry та sandbox залишаються джерелом істини.

## Структура

- `src/main.rs` — запуск Tokio runtime, конфігурація, вибір stdio/HTTP.
- `src/lib.rs` — CLI arguments та публічні модулі.
- `src/protocol.rs` — структури JSON-RPC request/response і валідація.
- `src/server.rs` — MCP dispatcher, initialize, tools/list, tools/call.
- `src/http.rs` — Axum HTTP transport, TLS і diagnostics endpoints.
- `src/config.rs` — allow-list, ліміти, access mode, tool categories.
- `src/validation.rs` — `cap_std` sandbox та symlink/path перевірки.
- `src/actions/` — реалізації filesystem, search, compression, crypto, CSV.
- `src/tools.rs` + `tools.json` — registry, категорії та JSON Schema.
- `src/errors.rs` — внутрішні помилки й відображення у JSON-RPC codes.

## MCP protocol implementation

Підтримувалися:

- `initialize` з negotiation версій;
- `ping`;
- `notifications/*`;
- `tools/list`;
- `tools/call`;
- пусті `prompts/list` та `resources/list`;
- `CallToolResult.content`, `structuredContent`, `isError`;
- ревізії MCP `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05`.

Сильні сторони:

- JSON-RPC envelope валідований окремо від tool arguments;
- notification не породжує JSON-RPC response;
- execution errors повертаються як `isError: true`;
- initialize не рекламував неіснуючі capabilities.

Прогалини:

- `resources/read` був відсутній;
- `resources/list` завжди був порожнім;
- не було `resources/templates/list`;
- capability `resources` не рекламувався;
- tool descriptor не посилався на MCP Apps UI resource.

## Transport layer

### stdio

Реалізація коректно обмежує розмір рядка, повторно використовує buffers,
не пише protocol messages у stderr і зберігається без змін.

### legacy HTTP

`POST /rpc` приймав один JSON-RPC request і повертав JSON response.
Цей endpoint залишено для backward compatibility.

### Виявлена прогалина

Streamable HTTP потребує єдиного `/mcp` endpoint з `POST` і `GET`,
JSON/SSE content negotiation, `MCP-Session-Id`, protocol-version header
та перевірку `Origin`.

## Tool registry

`tools.json` є декларативним registry, а `src/tools.rs` задає категорії.
`Config::tools_list_bytes` кешує відфільтрований `tools/list`.

Tool exposure безпечний за замовчуванням: жодна категорія не активна,
доки оператор явно не передасть `--enable-*`.

Для Apps UI до browser-oriented tools потрібно додати:

- `_meta.ui.resourceUri`;
- compatibility alias `_meta["openai/outputTemplate"]`;
- коректні annotations;
- аналогічну `_meta` у tool result.

## Filesystem sandbox

Sandbox використовує:

- canonicalization;
- PathTrie allow-list;
- `cap_std::fs::Dir`;
- capability-relative operations;
- заборону symlink components за замовчуванням;
- повторну перевірку canonical destination;
- file-size та decompression limits.

Цей sandbox придатний як security boundary для Resources API. Новий
`file://` resolver зобов'язаний проходити через `Config::sandbox()` і
не може напряму довіряти URI.

## Authentication

Було реалізовано лише опціональний статичний bearer token для HTTP.
Порівняння токенів виконується constant-time через SHA-256.

Цього достатньо для приватного MVP за reverse proxy, але недостатньо для
production ChatGPT user authentication. Повний OAuth має включати:

- authorization code + PKCE S256;
- protected-resource metadata;
- authorization-server metadata;
- `resource`/audience binding;
- access/refresh token rotation;
- per-tool security schemes;
- scope validation;
- `WWW-Authenticate` і `mcp/www_authenticate`.

Небезпечна «локальна» OAuth реалізація без identity provider, consent,
key rotation та token revocation не рекомендована.

## HTTP server

Позитивні властивості:

- loopback bind за замовчуванням;
- optional rustls TLS;
- request body limit;
- request timeout;
- `/health`, `/version`, `/info`, `/tools`;
- попередження при unauthenticated non-loopback bind.

Необхідні доповнення:

- `/mcp` Streamable HTTP;
- SSE;
- session lifecycle;
- Origin validation;
- protocol version validation;
- OAuth discovery у MVP-2.

## Error handling

`MCSError` відображає parse/invalid request/method/params та filesystem
помилки у стабільні JSON-RPC codes. Tool execution errors навмисно
перетворюються у `CallToolResult.isError`.

Для Resources доцільно окремо відрізняти «resource not found» (`-32002`)
від загального filesystem error у майбутньому. У MVP збережене чинне
mapping для backward compatibility.

## Security review

### Уже захищено

- path traversal після canonicalization;
- symlink escape за замовчуванням;
- write/delete категорії opt-in;
- readonly policy;
- bounded request/file/decompression sizes;
- constant-time static token comparison;
- TLS support.

### Додано в MVP-1

- file URI percent decoding до sandbox validation;
- canonical root validation під час recursive resource scan;
- hidden files не рекламуються;
- resource list pagination та верхня межа;
- `Origin: null` заборонено;
- same-origin або explicit `MCP_ALLOWED_ORIGINS`;
- session UUID та session validation;
- `MCP-Protocol-Version` validation;
- destructive tools не отримують UI template автоматично.

### Залишається для MVP-2/MVP-3

- OAuth, scopes і per-user sandbox;
- refresh-token persistence та revocation;
- audit JSONL із tamper-resistant rotation;
- quotas/rate limits;
- distributed session store для кількох replicas;
- explicit CSRF/state storage для authorization endpoint;
- security tests для Windows junction/reparse points;
- policy isolation crypto/compression output paths.

## Обрана реалізація

Ця гілка реалізує MVP-1 та deployment foundation:

1. Streamable HTTP `/mcp` з JSON/SSE і sessions.
2. Legacy `/rpc` без змін контракту.
3. `resources/list`, `resources/read`, `resources/templates/list`.
4. `file://` resources з MIME, size, preview, modification time.
5. `ui://filesystem/browser-v1.html`.
6. File Browser UI з directory listing, search і preview.
7. Apps UI metadata у descriptors/results.
8. Docker, Compose, environment template, CI та документацію.

OAuth, multi-user isolation і audit logging навмисно винесені у MVP-2:
їх не слід позначати production-ready до інтеграції з повноцінним
authorization server.
