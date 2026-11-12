# trackwright-cli

The `trackwright` command: every operation of the trackwright catalog as a subcommand,
`init` to start a tracker, and `mcp` to serve the catalog to an MCP client. It runs the
standalone host from `trackwright-core`, so actors are self-declared and the project's
policy binds honest agents only; see the
[harness integration guide](../trackwright-core/docs/harness-integration.md).

```sh
cargo install --path crates/trackwright-cli
```

## Usage

```text
trackwright <operation> [--field value ...] [--json] [--db PATH] [--actor ACTOR]
trackwright <operation> --help
trackwright init --key KEY [--name NAME]
trackwright mcp --actor ACTOR
```

- **Operations** are the catalog's names, with `-` or `_` between words: `criterion-add`
  and `criterion_add` are one operation. `trackwright --help` lists them with one line each.
- **Fields** are flags named after the operation's fields, `-` again standing for `_`:
  `--request-id` sets `request_id`. A field takes the JSON type the catalog declares, so
  `--version 3` is a number and a flag field takes `true` or `false`. A list field, such
  as the `accept` of `import_apply`, takes its flag once per item. A value that starts with
  `--` goes as `--field=VALUE`. `--help` after an operation lists its fields, as in
  `trackwright close --help`, and [schema.md](../trackwright-core/docs/schema.md) has
  them all.
- **`--json`** prints the reply envelope as one line. Without it the CLI prints a short
  text for a person.
- **`--help`** (or `-h`) anywhere explains the command instead of running it.

The CLI checks only the command line; the core checks every value and answers with the
same envelope and codes as over MCP.

## Actor

Every call but `help` and a plain `trackwright schema` runs as an actor, named by
`--actor` or else `TRACKWRIGHT_ACTOR`. `owner` is the project's owner, `engine:<source>`
a trusted automation such as `engine:ci`, and any other name an agent, such as
`agent:claude`. Nothing checks the name.

## Database

A call finds its database by `--db PATH`, then `TRACKWRIGHT_DB`, then
`.trackwright/tracker.db` in the working folder or the nearest folder above that holds
one. A symlink as the database or as a `.trackwright` folder on the way is refused, never
followed.

```sh
TRACKWRIGHT_ACTOR=owner trackwright init --key SHOP --name Shop
```

`init` adds the project `SHOP` (2 to 8 letters A-Z; the name defaults to the key) and
creates the database if it is missing. Without `--db` it uses
`./.trackwright/tracker.db`, and it refuses while a folder above already holds a tracker
that the new one would hide; pass `--db` to make a separate one anyway.

## Exit status

- `0`: the call was done.
- `1`: the core or the CLI refused it; the reply says why, with its `code`.
- `2`: the command line could not be read; one line on stderr says what to change.

## Examples

```sh
trackwright ready --project SHOP --json
trackwright show --id SHOP-1
trackwright claim --id SHOP-1 --version 1 --request-id claim-shop-1
trackwright move --id SHOP-1 --version 2 --status in_review
trackwright close --help
trackwright schema
```

## MCP

`trackwright mcp --actor ACTOR` serves the catalog over stdin and stdout, with the
database found as above, until the client closes its input. Only JSON-RPC reaches stdout;
a failure to start goes to stderr. See
[trackwright-mcp](../trackwright-mcp/README.md) for the protocol.

## License

Apache-2.0.
