# Harness integration guide

How to give a coding agent a trackwright tracker: on its own with the `trackwright` command
and its MCP server, or embedded in an application that is the tracker's host.

## 1. A tracker on its own

Build the command from this repository and create a tracker in the project's folder. Every
call names its actor with `--actor` or `TRACKWRIGHT_ACTOR`; the owner is `owner`.

```sh
cargo install --path crates/trackwright-cli
cd ~/projects/shop
TRACKWRIGHT_ACTOR=owner trackwright init --key SHOP --name Shop
```

`init` creates `.trackwright/tracker.db` in the working folder. Later calls find it there or
in the nearest folder above that holds one, or use `--db PATH` or `TRACKWRIGHT_DB`. A
symlink as the database or as a `.trackwright` folder on the way is refused, never followed.

The owner writes a ticket, its acceptance criterion and the check that proves it. A new
ticket starts in `backlog`; `ready` lists `todo` and `in_progress` work, so this one starts
in `todo`:

```sh
export TRACKWRIGHT_ACTOR=owner
trackwright create --project SHOP --title "Fix the login redirect" --kind bug --status todo
trackwright criterion-add --id SHOP-1 --text "Signing in returns to the page it started from"
trackwright check-add --id SHOP-1 --position 1 --kind ci --ref login-redirect
```

An agent takes unblocked work, claims it and reads it. A write to a ticket names the
`version` its last read returned, so two agents never overwrite each other; criteria and
checks keep versions of their own, so the new ticket is still at version 1:

```sh
export TRACKWRIGHT_ACTOR=agent:claude
trackwright ready --project SHOP
trackwright claim --id SHOP-1 --version 1
trackwright show --id SHOP-1 --json
```

Evidence for a `ci` or `manual` check comes from the owner or an engine actor, such as the
CI job that ran it. Under the default policy, `close = evidence`, the ticket closes itself
as `done` once every criterion is met or waived:

```sh
TRACKWRIGHT_ACTOR=engine:ci trackwright evidence-record --id SHOP-1 --position 1 --kind ci --ref login-redirect --result pass
```

A project can let agents close their own work instead of recording evidence. These two
commands replace the evidence step above: once passing evidence has closed the ticket, a
hand close is refused as `ticket_closed`. Under `close = evidence` only the owner closes a
ticket by hand, so the owner sets the policy first, and the agent closes the claimed ticket
at the version its claim returned:

```sh
TRACKWRIGHT_ACTOR=owner trackwright policy-set --project SHOP --key close --value agents
TRACKWRIGHT_ACTOR=agent:claude trackwright close --id SHOP-1 --version 2
```

`trackwright --help` lists the operations and `trackwright close --help` lists the fields
of one. With `--json` every call prints the reply envelope; without it, a short text. The
exit status is 0 when the call was done, 1 when it was refused and 2 for a command line the
CLI could not read.

## 2. Connecting an MCP client

`trackwright mcp` serves the whole catalog over stdio, one MCP tool per operation, named
after it: `ready`, `claim`, `close` and the rest. Each tool's input schema is the
operation's arguments from [schema.md](schema.md), and each result is the reply envelope as
text, flagged as an error when the call was refused. The actor is fixed when the server
starts; no tool call can change it. Give each agent its own actor name.

Name the database with `TRACKWRIGHT_DB` so the server finds it whatever folder the client
starts it in.

### Claude Code

```sh
claude mcp add --transport stdio --env TRACKWRIGHT_DB=/home/me/projects/shop/.trackwright/tracker.db trackwright -- trackwright mcp --actor agent:claude
```

Or check the server into the project as `.mcp.json`:

```json
{
  "mcpServers": {
    "trackwright": {
      "command": "trackwright",
      "args": ["mcp", "--actor", "agent:claude"],
      "env": {"TRACKWRIGHT_DB": "/home/me/projects/shop/.trackwright/tracker.db"}
    }
  }
}
```

### Codex

In `~/.codex/config.toml`:

```toml
[mcp_servers.trackwright]
command = "trackwright"
args = ["mcp", "--actor", "agent:codex"]
env = { TRACKWRIGHT_DB = "/home/me/projects/shop/.trackwright/tracker.db" }
```

### Any other client

The server speaks MCP JSON-RPC over stdio, one message or batch per line, in protocol
versions 2024-11-05, 2025-03-26, 2025-06-18 and 2025-11-25. A session that lists the tools
and asks for ready work:

```json
{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "example", "version": "1.0.0"}}}
{"jsonrpc": "2.0", "method": "notifications/initialized"}
{"jsonrpc": "2.0", "id": 2, "method": "tools/list"}
{"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "ready", "arguments": {"project": "SHOP"}}}
```

## 3. What standalone identity is worth

On its own, trackwright cannot tell who is calling. The actor is self-declared: whoever
starts `trackwright` or `trackwright mcp` names it, and nothing checks the name. An agent
that passes `--actor owner` gets the owner's rights. The project's policy (who may create,
edit and close tickets, and who records evidence) therefore binds honest agents only. It
keeps cooperating agents in their lanes; it is not a security boundary. When identity has
to hold, run trackwright inside a host that sets each actor itself, as below.

## 4. Embedding trackwright in a host

A host is any type that implements the four traits of `trackwright_core::host`:

- `Links`: the link kinds it accepts and what it knows of each target.
- `Evidence`: whether a check is satisfied, from its own records.
- `Authority`: whether an actor may run an operation or record evidence, and whether it
  sets actors itself.
- `Paths`: where `export` and `import_preview`/`import_apply` may touch files.

For permission the core asks only `actor_may`, before every operation: who may create,
edit, close, plan, import and set policy holds only when the host applies the project's
policy there. The example applies the host's own rule first, then the policy through
`policy::load` and `policy::permits`, as the standalone host does. The core applies the
rest of the policy itself, whatever the host does: the starting status under
`create = triage`, the refusal of an agent's own `met` under `close = evidence`, owner-only
waiving, and the cycle settings. The host names its actors so the policy reads their roles: `owner` for the
project's owner, `engine:<source>` for its own automation and any other name for an agent.

The core tables live in the host's own SQLite database. The host migrates them in a
transaction of its own, and runs every operation in a transaction it commits when the reply
is `ok`, so a ticket write and the host's own write land together or not at all:

```rust
use rusqlite::{Connection, Transaction};
use std::path::{Path, PathBuf};
use trackwright_core::host::{
    self, Authority, CheckView, Evidence, EvidenceState, LinkKind, LinkState, Links, Paths,
    TicketView,
};
use trackwright_core::model::{Actor, OpKind, ProjectKey, Refusal, codes};
use trackwright_core::store::StoreError;
use trackwright_core::{ops, policy, requests::Request, schema};

const KINDS: &[LinkKind] = &[LinkKind { name: "branch", label: "Branch" }];

struct Shop {
    exports: PathBuf,
}

impl Links for Shop {
    fn kinds(&self) -> &[LinkKind] {
        KINDS
    }

    fn resolve(
        &self,
        _tx: &Transaction<'_>,
        _project: &ProjectKey,
        _kind: &str,
        target: &str,
    ) -> Result<LinkState, Refusal> {
        Ok(LinkState { live: true, label: target.to_owned(), state: "open".to_owned() })
    }
}

impl Evidence for Shop {
    fn check(
        &self,
        _tx: &Transaction<'_>,
        _project: &ProjectKey,
        _check: &CheckView,
    ) -> Result<EvidenceState, Refusal> {
        Ok(EvidenceState { satisfied: false, detail: "no run yet".to_owned() })
    }
}

impl Authority for Shop {
    fn actor_may(
        &self,
        tx: &Transaction<'_>,
        project: &ProjectKey,
        actor: &Actor,
        op: OpKind,
        ticket: Option<&TicketView>,
    ) -> Result<(), Refusal> {
        if op == OpKind::ImportApply && actor.as_str() != "owner" {
            return Err(Refusal::host("shop_import_owner_only", "only the owner imports"));
        }
        let policy = policy::load(tx, project).map_err(|error| match error {
            StoreError::Refused(refusal) => refusal,
            other => Refusal::new(codes::STORE_FAILED, other.to_string()),
        })?;
        policy::permits(&policy, actor, op, ticket)
    }

    fn actors_verified(&self) -> bool {
        true
    }

    fn evidence_may(
        &self,
        _tx: &Transaction<'_>,
        _project: &ProjectKey,
        actor: &Actor,
        _check: &CheckView,
    ) -> Result<(), Refusal> {
        match actor.as_str() {
            "owner" => Ok(()),
            _ => Err(Refusal::host("shop_evidence_owner_only", "only the owner records evidence")),
        }
    }
}

impl Paths for Shop {
    fn resolve_path(
        &self,
        _tx: &Transaction<'_>,
        _project: &ProjectKey,
        _actor: &Actor,
        _op: OpKind,
        requested: &Path,
    ) -> Result<PathBuf, Refusal> {
        host::confined(&self.exports, requested)
            .map_err(|why| Refusal::host("shop_path_refused", why))
    }
}

fn ready(
    db: &mut Connection,
    shop: &Shop,
    caller: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let tx = db.transaction()?;
    schema::migrate(&tx)?;
    let actor = Actor::new(caller)?;
    let request = Request::parse("ready", br#"{"project": "SHOP"}"#)?;
    let reply = ops::dispatch(&tx, shop, &actor, request);
    if reply.is_ok() {
        tx.commit()?;
    }
    Ok(reply.json().to_owned())
}
```

In a real host each method reads the host's own tables through the transaction it is given.
The `caller` is the host's own identity of whoever asked, never a name the caller typed.
A refused operation leaves the core's tables as they were.

When something a link points at changes, such as a change set being accepted, the host
calls `trackwright_core::criteria::notify` with a `HostEvent` in the same transaction. The
tracker asks the host's `Evidence` again for the checks of every open ticket linked to that
target, and closes as `done` a ticket whose criteria are all met or waived, when the
project's close policy is `evidence`.

To serve the same host over MCP, open a `trackwright_mcp::Session` with a connection to the
migrated database (outside any transaction), the host and the client's actor, and call
`trackwright_mcp::serve` with the client's input and output.
