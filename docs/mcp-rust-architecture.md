# MCP architecture

Restream provides a Rust `restream-mcp` sidecar that exposes the agent-plane
workflow over MCP. It calls the product-native `/api/v1/agent/*` HTTP surface;
it does not bypass approval or mutate raw control-plane routes.

## Contents

- [Current shape](#current-shape)
- [Tool contract](#tool-contract)
- [Build and run](#build-and-run)
- [Authentication and compatibility](#authentication-and-compatibility)
- [Feature boundaries](#feature-boundaries)
- [Deferred embedded mode](#deferred-embedded-mode)
- [Source ownership](#source-ownership)

## Current shape

```mermaid
flowchart LR
    Client["MCP client"] -->|"streamable HTTP /mcp or stdio"| Sidecar["restream-mcp"]
    Sidecar --> Catalog["MCP handlers and tool catalog"]
    Catalog --> Backend["HTTP AgentBackend"]
    Backend -->|"/api/v1/agent/*"| Server["restream"]
    Server --> Core["Agent planning, approval, apply, and verify"]
```

The sidecar supports streamable HTTP and stdio transports. The default HTTP
bind is loopback. Tool handlers depend on the shared `AgentBackend` trait, and
the current runnable binary selects `HttpAgentBackend`.

## Tool contract

The catalog covers capabilities and context reads, investigation and planning,
validation and graph preview, and the approval-gated operation lifecycle. Tool
names and JSON schemas are defined once in `src/agent_mcp/tools.rs`; clients can
inspect the compiled catalog instead of relying on a duplicated list:

```sh
./restream-mcp --print-tools
```

MCP transport code validates and dispatches requests. Agent-plane code remains
the authority for redaction, plan validation, approval, apply, and verification
semantics. Operational guidance is in
[Agent-plane integration](agent-plane-integration.md).

## Build and run

Build the server with the sidecar's required features:

```sh
scripts/build/resource-limit.sh cargo build \
  --bin restream-mcp \
  --features mcp-server,mcp-http-backend
```

With `restream` listening on its default HTTP address, start streamable HTTP:

```sh
RESTREAM_AGENT_SESSION_COOKIE='session=<value>' \
  target/debug/restream-mcp --bind 127.0.0.1:4040
```

The MCP endpoint is `/mcp`. For a client that launches its server over stdio:

```sh
RESTREAM_AGENT_SESSION_COOKIE='session=<value>' \
  target/debug/restream-mcp --stdio
```

`RESTREAM_AGENT_BASE_URL` changes the target server from
`http://127.0.0.1:3030`.

## Authentication and compatibility

The HTTP backend can forward one of these credentials:

- `RESTREAM_AGENT_SESSION_COOKIE` for the server's session cookie;
- `RESTREAM_AGENT_BEARER_TOKEN` for deployments that terminate bearer auth at
  a compatible boundary.

If both are present, the session cookie is selected. Empty values mean no
credential. Transport deployment must protect these values and use TLS when the
server is remote.

Before serving, the sidecar checks the target build identity and capabilities.
`RESTREAM_MCP_VERSION_CHECK` accepts `strict` (the default), `warn`, or `off`.
Use `off` only for deliberate compatibility testing.

For streamable HTTP, `RESTREAM_MCP_ALLOWED_ORIGINS` is a comma-separated origin
allowlist. An empty list does not grant a wildcard browser origin.

## Feature boundaries

Cargo features keep the optional surface explicit:

| Feature | Adds |
|---|---|
| `agent-plane` | Agent read/planning API |
| `agent-execution` | Approval-gated operation execution |
| `mcp-core` | Shared MCP-facing backend and type contract |
| `mcp-server` | MCP transport/server implementation |
| `mcp-http-backend` | HTTP adapter to `/api/v1/agent/*` |
| `mcp-embedded` | In-process backend scaffold |

The `restream-mcp` binary requires the server and HTTP-backend feature set to
run. The exact dependency edges and binary requirements are owned by
`Cargo.toml`.

## Deferred embedded mode

`InProcessBackend` exists to preserve a future embedded boundary, but only its
capabilities call is wired. Other operations return `NotYetImplemented`, and
the main `restream` binary does not mount an MCP transport. Therefore embedded
MCP is not a supported deployment mode today.

Completing it requires routing the existing application workflows through the
backend without duplicating HTTP handler policy. Until that happens, use the
HTTP-backed sidecar.

## Source ownership

| Path | Responsibility |
|---|---|
| `src/agent_core/` | Shared backend contract, request types, and errors |
| `src/agent_backends/http.rs` | Current HTTP-backed execution adapter |
| `src/agent_backends/in_process.rs` | Deferred embedded adapter scaffold |
| `src/agent_mcp/` | Tool catalog, dispatch, and transports |
| `src/bin/restream-mcp.rs` | CLI, environment, compatibility check, backend selection |
| `src/agent_plane.rs`, `src/agent_execution.rs` | Product-native agent behavior |
| `Cargo.toml` | Feature graph and binary requirements |
