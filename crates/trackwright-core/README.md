# trackwright-core

An agent-first ticket tracker core: the model, a SQLite store, an event log, text rules and
the traits a host implements. Agents and people drive it through one catalog of operations,
each a JSON request answered by one JSON reply envelope, so the same calls work from a
command line, an MCP client or a program that embeds the tracker.

- `trackwright-core` (this crate): model, store, operations and host traits.
- `trackwright-cli`: the `trackwright` command, which also launches the MCP server.
- `trackwright-mcp`: the MCP stdio server over the same catalog.

## What it tracks

- **Projects** have a key of 2 to 8 capital letters, such as `CA`.
- **Tickets** have a short id such as `CA-42`, a kind (`feature`, `bug`, `chore` or
  `spike`), a status (`triage`, `backlog`, `todo`, `in_progress`, `in_review`, `done` or
  `cancelled`), a priority, an optional parent, a release and a cycle.
- **Acceptance criteria** sit on a ticket, in order. Each criterion can carry **checks** (a
  `test`, a `change`, a `manual` check or a `ci` run), and recorded **evidence** says
  whether a check passed.
- **Links** tie a ticket to things outside the tracker, such as a branch, a change set or a
  task. The host decides which link kinds exist and what state each target is in.
- **Dependencies** say which tickets block which; `ready` lists only unblocked work.
- **Releases** and **cycles** plan work over time; `roadmap` shows them.
- **Policy** per project decides who may create, edit and close tickets and how cycles roll
  over; `policy_get` shows every setting with its options.

Every change to a ticket's own fields bumps its `version`, and each criterion keeps a
version of its own. A write to an existing ticket or criterion names the version it last
read, and a newer stored version refuses it as `stale_version`, so two agents never
overwrite each other. A write may carry a `request_id`; repeating it with the
same fields returns the first reply instead of writing twice.

## Operations

The catalog in `src/catalog.rs` lists 30 operations with their fields, bounds and one line
each. The same catalog drives request parsing, the CLI flags, the MCP tools and the
published schema:

- [docs/schema.md](docs/schema.md): every operation and field, generated from the catalog.
- [schema/v1.json](schema/v1.json): the same as JSON Schema, API version 1.

A reply is one envelope: `ok` with `data`, up to five `next` actions and a `cursor` for the
next page, or a refusal with a stable `code` and a one-line `message`. Reads are bounded by
the reply size, so a long list comes back in pages.

The two shapes, with each operation's own `data` left out:

```json
{"api": 1, "ok": true, "data": {}, "next": [{"op": "claim", "id": "CA-7", "why": "..."}], "cursor": null}
{"api": 1, "ok": false, "code": "stale_version", "message": "...", "next": []}
```

## Identity and roles

Every call runs as one actor, a short name such as `owner`, `agent:a` or `engine:landing`.
Policy reads three roles from it:

- `owner` is the project's owner.
- `engine:<source>` is a host's trusted automation.
- Every other actor is an agent.

Names starting with `host:` belong to the core itself; no caller can act as one.

## Using it

- **On its own:** install `trackwright-cli` and run `trackwright init --key CA`. The
  database is `.trackwright/tracker.db` in that folder. Agents call the CLI or connect
  through `trackwright mcp`. In this mode actors are self-declared, so the project's policy
  binds honest agents only. See [docs/harness-integration.md](docs/harness-integration.md).
- **Inside an application:** implement `host::Host` (links, evidence, authority and file
  paths), migrate the core tables into your own database with `schema::migrate`, and call
  `ops::dispatch` in your own transactions. The host then sets each actor from what it
  knows of the caller. The guide has a full example.

## Bounds

Every input has a limit: a request is at most 16 KiB, a reply 8 KiB by default and 64 KiB at
most, and every text field, list and page has its own maximum, listed in the schema. A call
past a bound is refused with a code, never truncated in silence.

## License

Apache-2.0.
