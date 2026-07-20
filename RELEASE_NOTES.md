# Release notes draft

## OpenAI Apps SDK / ChatGPT compatibility update

This release improves MCP and JSON-RPC compatibility for ChatGPT and OpenAI Apps SDK deployments while preserving existing stdio, HTTP, CLI, tool names, and tool schemas.

Highlights:

- Honest MCP initialization capabilities and richer `serverInfo` metadata.
- Standard JSON-RPC 2.0 error handling for malformed requests and unknown methods.
- Structured MCP tool results with `content`, `structuredContent`, and `isError` handling.
- Apps SDK-compatible tool annotations and generated titles/output schemas.
- Diagnostic HTTP endpoints for health, version, server info, and tool inspection.
- Configurable request size limits for both stdio and HTTP transports.

Not included in this release:

- ChatGPT UI resources or Apps SDK HTML resources.
- Authentication flows beyond the existing optional HTTP bearer token.
- MCP Inspector integration.
