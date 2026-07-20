# Changelog

## Unreleased

### Added

- Added OpenAI Apps SDK / ChatGPT-oriented MCP compatibility improvements for initialization metadata, empty prompts/resources list handlers, structured tool responses, and Apps SDK-compatible tool descriptor annotations.
- Added HTTP diagnostics endpoints: `GET /health`, `GET /version`, `GET /info`, and `GET /tools`.
- Added configurable HTTP JSON body limit via `--max-http-body-bytes`.

### Changed

- Improved JSON-RPC 2.0 validation and standard error-code handling for parse errors, invalid requests, invalid params, unknown methods, and notifications.
- Expanded `serverInfo` with homepage, repository, and license metadata.
- Updated README documentation for ChatGPT compatibility and HTTP diagnostics.
