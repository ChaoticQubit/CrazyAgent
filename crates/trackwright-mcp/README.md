# trackwright-mcp

An MCP server for a trackwright tracker over stdio: every operation of the catalog is one
tool, and every tool call answers with the core's reply envelope. The `trackwright mcp`
command runs it for a standalone tracker; a host application runs it with its own host.

## Launching

```sh
trackwright mcp --actor agent:claude
```

The command finds its database as every `trackwright` call does (`--db`, then
`TRACKWRIGHT_DB`, then `.trackwright/tracker.db` in the working folder or above). The
[harness integration guide](../trackwright-core/docs/harness-integration.md) has the
Claude Code and Codex configuration.

The actor is fixed when the server starts and no tool call can change it. Started by
`trackwright mcp`, the actor is self-declared, so the project's policy binds honest agents
only; a host that names each client itself makes it hold.

## Tools

- `tools/list` returns one tool per operation: its `name` is the operation's name, its
  `description` the catalog's one line, and its `inputSchema` the operation's arguments as
  JSON Schema, the same as `schema/v1.json` in `trackwright-core`.
- `tools/call` runs the operation in a transaction of its own, committed when the reply is
  `ok` and rolled back otherwise. The result holds the envelope as one text item, and
  `isError` is true when the call was refused.
- An unknown tool, or `arguments` that are not an object, is a JSON-RPC `Invalid params`
  error, not a tool result. Missing `arguments` count as an empty object.

A session that lists the tools and asks for ready work:

```json
{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "example", "version": "1.0.0"}}}
{"jsonrpc": "2.0", "method": "notifications/initialized"}
{"jsonrpc": "2.0", "id": 2, "method": "tools/list"}
{"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "ready", "arguments": {"project": "SHOP"}}}
{"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "claim", "arguments": {"id": "SHOP-1", "version": 1}}}
```

## Protocol

- JSON-RPC 2.0, one message or batch per line on stdin, one reply per line on stdout.
  Notifications get no reply; a batch gets one array of replies.
- Methods: `initialize`, `ping`, `tools/list` and `tools/call`.
- Protocol versions 2024-11-05, 2025-03-26, 2025-06-18 and 2025-11-25. A client that asks
  for another is offered 2025-11-25.
- A line holds at most 32 KiB, twice the core's request bound; a longer one is answered
  `Request too large` and skipped. A batch holds 1 to 32 requests.
- The server runs until the client closes its input.

## Embedding

```rust
use trackwright_core::model::Actor;
use trackwright_mcp::{Session, serve};

fn serve_client(
    connection: rusqlite::Connection,
    host: impl trackwright_core::host::Host,
    client: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let session = Session::new(connection, host, Actor::new(client)?);
    serve(std::io::stdin().lock(), std::io::stdout().lock(), &session)?;
    Ok(())
}
```

The connection is open on a database that already holds the migrated core schema
(`trackwright_core::schema::migrate`) and is outside any transaction. The actor is the
host's own identity of the client and never one of the core's reserved `host:` actors.

## License

Apache-2.0.
