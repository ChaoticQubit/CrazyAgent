# Harness manifests

A harness is a coding-agent program that CrazyAgent runs agents inside, such as Codex or Claude Code. Each harness the app knows is described by one data file, its manifest:

`crates/crazyagent/harness-manifests/<id>.yaml`

The engine reads the bundled manifests once, builds its harness registry from them and serves the harness catalog from that registry.

This guide says what a manifest may hold and what the engine does with each part. Every YAML example is copied from a file in the repository: one of the 18 bundled manifests, or one of the stand-in manifests the tests use.

## Where things are

| Part | File |
| --- | --- |
| Strict YAML reader | `crates/crazyagent/src/app_plugins/yaml.rs` |
| Manifest shape, top-level checks and refusal codes | `crates/crazyagent/src/harnesses/manifest.rs` |
| Registry, and the list of bundled manifests in catalog order | `crates/crazyagent/src/harnesses/registry.rs` |
| `accounts` block | `crates/crazyagent/src/harnesses/accounts.rs` |
| `home` block and the private home | `crates/crazyagent/src/harnesses/home.rs` |
| Names a manifest may not set | `crates/crazyagent/src/harnesses/env_names.rs` |
| `mcp` and `instructions` blocks | `crates/crazyagent/src/harnesses/render.rs` |
| Drivers for running without a terminal | `crates/crazyagent/src/harnesses/drivers/` |
| Event rules | `crates/crazyagent/src/harnesses/drivers/rules.rs` |
| `terminal` block | `crates/crazyagent/src/harnesses/interactive/settings.rs` |
| Record readers and record paths | `crates/crazyagent/src/harnesses/records/` |
| `import` block | `crates/crazyagent/src/skills/import.rs`, `crates/crazyagent/src/harnesses/records/past.rs` |
| `update` block | `crates/crazyagent/src/harnesses/update/recipes.rs` |
| The 18 manifests | `crates/crazyagent/harness-manifests/` |
| Stand-in manifests for tests | `crates/crazyagent/tests/fixtures/harness/` |

## The 18 bundled manifests

In catalog order. Codex, Claude Code and RCLI are driven by the engine's own adapters and carry the blocks those adapters read. The other 15 are catalog rows only for now: each names its account kinds, its install recipe and a `cannot_start` reason, and holds no `piped`, `terminal`, `home` or `accounts` block yet.

| Id | Name | Account kinds | Today |
| --- | --- | --- | --- |
| `codex` | Codex | subscription, api_key, open_ai_compatible_url, local | Agent, master and research |
| `claude-code` | Claude Code | subscription, api_key | Agent, master and research |
| `opencode` | OpenCode | subscription, api_key, open_ai_compatible_url, local | Catalog row |
| `gemini-cli` | Gemini CLI | subscription, api_key | Catalog row |
| `fx` | fx by Vercel | api_key, open_ai_compatible_url, local | Catalog row; tools: "no documented switch to turn tools off" |
| `openclaw` | OpenClaw | subscription, api_key, local | Catalog row; tools: "tools are set in OpenClaw's own configuration" |
| `pi` | Pi | subscription, api_key, open_ai_compatible_url, local | Catalog row |
| `rcli` | RCLI | api_key, open_ai_compatible_url, local | Master only: "runs only as a CrazyAgent master", "runs without tools" |
| `goose` | Goose | api_key, open_ai_compatible_url, local | Catalog row |
| `aider` | Aider | api_key, open_ai_compatible_url, local | Catalog row; tools: "file edits cannot be switched off apart from a dry run" |
| `qwen-code` | Qwen Code | subscription, api_key, open_ai_compatible_url, local | Catalog row |
| `crush` | Crush | api_key, open_ai_compatible_url, local | Catalog row |
| `droid` | Factory Droid | subscription, api_key, open_ai_compatible_url | Catalog row |
| `copilot` | GitHub Copilot CLI | subscription | Catalog row |
| `cline` | Cline CLI | subscription, api_key, open_ai_compatible_url, local | Catalog row |
| `continue` | Continue CLI | api_key, open_ai_compatible_url, local | Catalog row |
| `openhands` | OpenHands CLI | api_key, open_ai_compatible_url, local | Catalog row; tools: "headless mode always runs its tools" |
| `kimi` | Kimi CLI | subscription, api_key | Catalog row |

Every catalog row's `cannot_start` reads "installed, but CrazyAgent cannot start it yet".

A full manifest, `claude-code.yaml`:

```yaml
schema: 1
kind: harness
id: "claude-code"
name: "Claude Code"
aliases:
  - "claude"
binaries:
  - "claude"
locations:
  - ".claude/local"
headless: "claude --print with stream-json input and output"
model_selection: true
account_kinds:
  - "subscription"
  - "api_key"
account_protocols:
  - "anthropic-messages"
limits: "claude"
version:
  sign_in_minimum: "2.1.268"
update:
  own:
    root: ".local/share/claude/versions"
    args:
      - "update"
    pointer:
      url: "https://downloads.claude.ai/claude-code-releases/latest"
      format: "plain-version"
      bytes_max: 64
  casks:
    - "claude-code"
  known_casks:
    - "claude-code@latest"
  npm:
    scope: "@anthropic-ai"
    name: "claude-code"
    package: "@anthropic-ai/claude-code"
    spec: "@anthropic-ai/claude-code@latest"
  guide: "https://code.claude.com/docs/en/setup"
roles:
  - "agent"
  - "master"
  - "research"
sub_agents: "teams"
home:
  owner_folders:
    - ".claude"
    - ".claude.json"
piped:
  driver: "claude-stream-json"
terminal:
  record:
    reader: "claude-session"
import:
  conversations: ".claude/projects/*/*.jsonl"
```

A catalog row, `pi.yaml`:

```yaml
schema: 1
kind: harness
id: "pi"
name: "Pi"
binaries:
  - "pi"
headless: "pi --mode json or --mode rpc over stdio"
model_selection: true
account_kinds:
  - "subscription"
  - "api_key"
  - "open_ai_compatible_url"
  - "local"
update:
  formulae:
    - "pi-coding-agent"
  npm:
    scope: "@earendil-works"
    name: "pi-coding-agent"
    package: "@earendil-works/pi-coding-agent"
    spec: "@earendil-works/pi-coding-agent"
    flags:
      - "--ignore-scripts"
  guide: "https://pi.dev/"
cannot_start: "installed, but CrazyAgent cannot start it yet"
```

The blocks a shared driver reads (`piped` beyond `driver`, `terminal` beyond `record.reader`, `home.env`, `rules`) appear in no bundled manifest yet. Their examples below come from the stand-ins in `crates/crazyagent/tests/fixtures/harness/`, which the tests load through the same reader.

## What the YAML reader accepts

It is a strict subset, the same one plugin files use.

- At most 16 KiB of UTF-8. Spaces to indent, never tabs.
- Maps of `key: value` lines, nested at most 8 deep. A key is letters, digits, `_`, `.` and `-`, at most 64 bytes.
- Lists hold single values only. A list of maps is refused. Where several named things are needed, use a map whose keys are the names.
- A value in quotes is always text. Without quotes, `true`, `false`, `null`, whole numbers and decimals are typed, so quote every text value. `minimum: 1.5` is a number and is refused; `minimum: "1.5"` is text.
- Anchors, aliases, tags, block scalars, flow maps and several documents in one file are refused.

Write no comment lines in a manifest. Explain here instead.

## Top-level keys

| Key | Needed | Meaning |
| --- | --- | --- |
| `schema` | yes | Always `1`. |
| `kind` | yes | Always `harness`. |
| `id` | yes | Catalog id: lower-case letters, digits and dashes, at most 64 bytes. The file is named after it. |
| `name` | yes | The product's plain name shown to the owner. |
| `aliases` | no | Other names a team plan or connection may store, such as `claude` for `claude-code`. |
| `binaries` | yes | Program names searched in order. No folder in a name. |
| `locations` | no | Extra install folders, relative to the home folder. |
| `headless` | yes | One sentence on how it runs without a terminal. |
| `model_selection` | yes | `true` or `false`. |
| `account_kinds` | yes | One or more of `subscription`, `api_key`, `open_ai_compatible_url`, `local`. The same words the apps use. |
| `account_protocols` | no | The wire formats of the endpoint accounts it runs on, such as `anthropic-messages`. See "Which account fits a harness". |
| `limits` | no | How the limits of a sign-in are read beyond its usage windows: `codex` or `claude`. Leave out when they are not reported. |
| `tools_limited` | no | The plain reason its tools cannot be switched off. Leave out when they can. |
| `cannot_start` | no | The plain reason CrazyAgent cannot start it. Leave out when it can. |
| `roles` | no | Any of `agent`, `master`, `research`. Leave out for a harness that can be none of them yet. |
| `sub_agents` | no | `teams`, `threads` or `app`. Default `app`: workers the app starts. |
| `piped` | no | How it runs without a terminal. See "Running without a terminal". |
| `terminal` | no | How its live terminal is run and read back. See "Running in a terminal". |
| `version.minimum` | no | The oldest version CrazyAgent drives: two to six numbers joined by dots, in quotes. |
| `version.sign_in_minimum` | no | The oldest version CrazyAgent can sign in with: two to six numbers joined by dots, in quotes. `2` and `1..2` are refused, because the version check cannot compare them. Leave out for a harness CrazyAgent never signs in to. |
| `update` | no | The install and update recipe. See "The update block". |
| `home` | no | Its folders in the owner's home and its private home. See "The home block". |
| `accounts` | no | How it is handed an account of each wire format. See "The accounts block". |
| `mcp`, `instructions` | no | Files written into its private home for a launch. See "Files written for a launch". |
| `import` | no | Files in the owner's home the owner may import from. See "The import block". |
| `rules` | no | Named event rule sets. See "Event rules". |

A key that is not in this table is refused. The blocks from `version` down are each read and checked by the part of the engine that uses them, in its own file.

At most 16 entries in any one list. A sentence or folder is at most 240 bytes; an id, alias or program name at most 64.

## Which account fits a harness

One rule, for every master, team agent and research job:

- A sign-in fits the harness it was made with, when that harness lists `subscription` under `account_kinds`.
- A ChatGPT sign-in also fits a harness other than Codex, Claude Code or RCLI that lists `openai-responses` by that full name under `account_protocols`. It reaches that harness only through the ChatGPT relay, never as a copy of the sign-in. A Claude subscription fits Claude Code only.
- An endpoint account, with a key or without, fits every harness that lists its wire format under `account_protocols`. The harness the account was stored with does not matter.

A name in `account_protocols` that ends in `*` stands for every format that starts with the text before it: `openai*`, or `*` alone for all. Only the engine's own adapters may use it. A harness on a shared driver lists each wire format by its full name, because each one needs its own `accounts` entry.

Codex lists `openai*`, Claude Code `anthropic-messages`, RCLI `openai*` and `anthropic-messages`.

### A ChatGPT sign-in on a listed harness

Listing `openai-responses` means the harness speaks the OpenAI Responses format: it sends `POST {relay_url}/responses` and streams the reply. On a ChatGPT sign-in, the engine starts a ChatGPT relay for the run. The harness's `accounts.openai-responses` entry gets three values:

- `{relay_url}`: the relay's loopback URL, which ends in `/backend-api/codex`.
- `{token}`: the relay's placeholder.
- `{model}`: the model chosen for the run.

The relay forwards to `https://chatgpt.com/backend-api/codex`, the backend Codex uses for a ChatGPT sign-in:

- It forwards only `POST /responses`, exactly. Every other path, method or query answers 404, because the access token opens the whole ChatGPT account.
- The engine adds `Authorization: Bearer` with the access token, and `chatgpt-account-id` when the sign-in has one.
- The harness's `cookie`, `openai-organization` and `openai-project` headers are dropped, as is every `chatgpt-` or `x-openai-` header it sends. No Codex client header is added.
- The body passes unchanged.
- The access token stays in engine memory. The engine renews it 5 minutes before it lapses, only through its own renewal. A sign-in that can no longer be renewed answers 401 with code `chatgpt_sign_in_expired`: "This ChatGPT sign-in has expired. Sign in again in Settings, Accounts."

The run never uses the sign-in's managed home, where the engine keeps Codex's own copy of the sign-in:

- It runs in the harness's own private home (see "The home block").
- The launch is refused (`launch_chatgpt_home_is_managed`) if the agent home and the managed home overlap: the same folder, either one inside the other, or a link to it. The record need not be there yet for this to apply.
- The launch is refused (`launch_account_home_holds_sign_in`) if the agent home or the private home holds `auth.json` or `.credentials.json`, whether as a file or as a link.
- The run's sandbox is made after routing, for the private home. A request that already carries a sandbox profile is refused (`launch_chatgpt_sandbox_premade`).
- The relay is tied to the sign-in it was read from. A relay or source of another sign-in is a wiring fault and stops on an assertion.

Workspace-routed, residency and FedRAMP accounts are not supported.

## What the catalog serves

Each catalog row carries, from the manifest: `terminal` (it has a record reader, so a live terminal can be read back), `master` and `research` (from `roles`), `sub_agents` (`teams`, `threads` or `app`) and `account_protocols` as written. The apps read these fields from the catalog row.

## The home block

```yaml
home:
  env_prefixes:
    - "STANDIN_ACP_"
  env:
    STANDIN_ACP_CONFIG: "{home}/config"
```

From `standin-acp.yaml`.

| Key | Meaning |
| --- | --- |
| `owner_folders` | The folders and files under the owner's home that hold this harness's own files, such as `.claude` and `.claude.json`. Each is relative to the home: no `~`, `.` or `..` part and no leading `/`. Agents never read them, and no project or sandbox folder may sit inside one. Every `import` path must lie inside one of them. |
| `env_prefixes` | At most 4 starts of variable names the harness reads as its own, such as `OPENCODE_`: capital letters, digits and `_`, starting with a letter and ending with `_`. A prefix that covers a name CrazyAgent owns is refused (see "Names a manifest may not set"). |
| `env` | At most 16 variables set on every launch. Each name starts with one of the manifest's own prefixes, or with `XDG_`. Each value is text in quotes, at most 240 bytes with no control character: plain text, or `{home}` followed only by a folder inside it. An `XDG_` value must be `{home}` itself or a folder inside it. `{home}` is the only placeholder filled here. |
| `set_home` | `true` sets `HOME` itself to the private home. |

A folder inside `{home}` is `/` and names of ASCII letters, digits, `.`, `_` and `-`, so no `:` can add a second folder to a list such as `XDG_CONFIG_DIRS`.

### The private home

A harness that runs on a shared driver runs in its own private home, one per account: `<agent home>/<harness id>/<account>`. Each folder on the way is made 0700, owned by the user CrazyAgent runs as, through steps that follow no link, so a link planted at any of the names is refused rather than followed. `{home}` in a manifest is this folder, never the owner's home.

## Names a manifest may not set

One shared rule covers the `home.env` names, the `home.env_prefixes` starts and the `accounts.<format>.env` names. These are names the engine sets itself on a launch, or names that decide which shell setup runs, which code a runtime loads or which server an install trusts. Names are compared in capital letters, so a lower-case spelling counts too.

Whole names:

`PATH`, `HOME`, `SHELL`, `TMPDIR`, `TMPPREFIX`, `USER`, `LOGNAME`, `TERM`, `LANG`, `TZ`, `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, `ENV`, `ZDOTDIR`, `PROMPT_COMMAND`, `SHELLOPTS`, `BASHOPTS`, `PS4`, `ELECTRON_RUN_AS_NODE`, `JAVA_TOOL_OPTIONS`, `JDK_JAVA_OPTIONS`, `GOFLAGS`, `GOPROXY`, `GOSUMDB`, `GONOSUMDB`, `GOINSECURE`, `GOPRIVATE`, `GONOPROXY`, `GOTOOLCHAIN`, `CURL_CA_BUNDLE`, `REQUESTS_CA_BUNDLE`.

Every name that starts with:

`CRAZYAGENT_`, `DYLD_`, `LD_`, `GIT_`, `SSL_`, `OPENSSL_`, `NPM_`, `PNPM_`, `YARN_`, `BUN_`, `CARGO_`, `RUST`, `CODEX_`, `CLAUDE_`, `UV_`, `COREPACK_`, `HOMEBREW_`, `LC_`, `XDG_`, `PYTHON`, `PERL`, `RUBY`, `GEM_`, `NODE_`, `BASH_`, `PIP_`.

- `home.env` may still set an `XDG_` folder, to the private home or a folder inside it.
- A prefix is refused when it would cover an owned name in either direction: an owned name or family starts with the prefix's stem, or the prefix starts with an owned family. So `GIT_`, `NPM_` and `PYTHON_` are refused, and so is `NODE_` or `XDG_`.
- `PIP_` lies inside `PI_`, a prefix a harness may keep. `PI_` stays allowed, and a prefix that starts with `PIP_` is refused. A `PIP_` name is refused wherever a name is set.

This rule runs when the manifest is read. It is not the launch guard: the engine sets its own git, home, TLS and shell temp settings after the harness's variables, so a manifest value never replaces one.

## The accounts block

A harness on a shared driver has one entry for each wire format it lists under `account_protocols`, named after that format, such as `accounts.openai-responses`. The engine's own adapters need none. A missing entry, or an entry for a format that is not listed, is refused.

An entry has one key the engine reads, `env`: at most 16 variables, each a name of capital letters, digits and `_` that the shared rule allows, set to text in quotes of at most 1024 bytes. The only placeholders an entry may hold are:

- `{relay_url}`: where the harness sends model requests. For an account with a key this is the loopback address of the engine's key relay; for a keyless account it is the account's own address.
- `{token}`: what the harness presents there. For a keyed account it is the relay's placeholder, for a keyless one a local placeholder. It is never a saved key.
- `{model}`: the model the account names.

No saved key is read when an account is handed over. A ChatGPT sign-in is handed over only through the ChatGPT relay.

## Files written for a launch

`mcp` and `instructions` each ask for one file written into the private home before a launch:

| Key | Meaning |
| --- | --- |
| `render` | How the file is written. `mcp`: `json-mcp-servers`, one JSON file holding an `mcpServers` map, the shape Claude Code reads, filled with the MCP servers the launch gives. `instructions`: `file`, the whole instruction text as one file. |
| `path` | `{home}/` and then 1 to 8 names joined by `/`, each ASCII letters, digits, `.`, `_` and `-`, none empty, `.` or `..`. It may not start with a name CrazyAgent keeps at the top of a private home (its launch state, home lock and key file), in any letter case. |

Each file is written 0600 and made new through steps that follow no link, so a link planted in the home is replaced or refused, never written through.

## Running without a terminal

`piped.driver` names how the engine talks to the harness without a terminal:

| Driver | For |
| --- | --- |
| `codex-app-server` | Codex, the engine's own adapter. |
| `claude-stream-json` | Claude Code, the engine's own adapter. |
| `rcli` | RCLI, the engine's own adapter. |
| `acp` | A harness that speaks the Agent Client Protocol over JSON-RPC lines on its standard input and output. |
| `jsonl-turn` | A harness that runs one process per turn and writes JSON lines. |
| `jsonl-rpc` | A harness that runs one long-lived process and speaks JSON lines both ways. |
| `text-turn` | A harness that runs one process per turn and gives no event stream. |

The shared drivers read these keys under `piped`. A blank cell means that driver does not read the key.

| Key | `acp` | `jsonl-turn` | `jsonl-rpc` | `text-turn` | Meaning |
| --- | --- | --- | --- | --- | --- |
| `argv` | yes | yes, with `{prompt}` | yes, without `{prompt}` | yes, with `{prompt}` | The arguments of the process. |
| `resume` | | yes | yes | yes | Arguments added to continue a session. Must hold `{session}`. |
| `tools_off` | yes | yes | yes | yes | Arguments added while tools are off, placed before the first `--` entry. |
| `rules` | | yes | yes | | The name of the rule set in `rules` that turns output lines into events. |
| `prompt` | | | yes | | One JSON object on one line, 1 to 1024 bytes, with exactly one `{prompt}` inside a JSON string. Each prompt is sent framed in it. |
| `interrupt` | | | yes | | One JSON object on one line, holding no value to fill. Sent to stop the running turn. |
| `reply` | | | | yes | For a harness that ends its output with one JSON object: the dotted path of the reply in it. Without it, the reply is the whole output. |
| `session` | | | | yes | The dotted path of the session id in that object. Needs `reply`. |

- Each argument is 1 to 1024 bytes of text.
- A `{prompt}` may not begin an argument before any `--` entry, so a prompt is never read as an option: write `--prompt={prompt}`, or put it after `--`.
- `{prompt}` may not stand in `resume` or `tools_off`.
- A `text-turn` turn keeps at most 1 MiB of output; a turn whose output passes it is stopped.
- The `acp` driver fills `{home}`, `{cwd}` and `{model}` in `argv`. It offers the agent no file or terminal service and no MCP server but the engine's own, which it hands over in the session it opens, and none while tools are off. It answers the agent's permission requests from the launch's tools switch. An ACP agent asks permission only when it chooses to, so an ACP manifest must name its own switch in `tools_off` or say why it cannot in `tools_limited`. An agent that needs its own sign-in first is refused with `launch_account_unsupported`.
- `jsonl-turn` and `jsonl-rpc` do not hand out `{relay_url}`, `{token}` or `{key_file}` in arguments; the account reaches the harness through its `accounts` entry.

An ACP harness, from `standin-acp.yaml`:

```yaml
piped:
  driver: "acp"
  argv:
    - "--home"
    - "{home}"
  tools_off:
    - "--no-tools"
```

A long-lived JSON lines harness, from `standin-jsonl-rpc.yaml`:

```yaml
piped:
  driver: "jsonl-rpc"
  argv:
    - "--home"
    - "{home}"
    - "--model"
    - "{model}"
  resume:
    - "--session"
    - "{session}"
  tools_off:
    - "--no-tools"
  prompt: '{"type":"prompt","message":"{prompt}"}'
  interrupt: '{"type":"abort"}'
  rules: "standin-events"
```

## Event rules

`rules` is a map of named rule sets. A `piped.rules` or `terminal.record.rules` value names one. Each set is a map of named rules, read in file order; for each JSON object the harness writes, the first rule whose conditions all hold decides what it means.

| Key | Meaning |
| --- | --- |
| `when` | At most 8 conditions: a dotted path and the value it must hold. A condition holds when the path reaches a text equal to it, or a number or flag whose JSON text equals it. A part of only digits indexes a list. |
| `emit` | What the object reports. One of `ready`, `turn_started`, `text`, `tool_use`, `waiting`, `interrupted`, `turn_finished`, `turn_error`, `usage`, `exited`. |
| `text` | The path of the text, needed for `text`. |
| `name` | The path of the tool name, needed for `tool_use`. |
| `session` | The path of the harness's session id. |
| `success` | The path of a flag that says whether a finished turn went well. |
| `tokens` | Up to 4 paths of token counts, in this order: input, output, cache read, cache write. |
| `rate_limited` | The path of a flag that marks an error as a rate limit, or of the error's message, which the engine reads for rate limit, usage limit, quota and HTTP 429 wording. |

At most 32 rules in a set. A path is 1 to 8 plain parts joined by dots, at most 128 bytes.

From `standin-jsonl-rpc.yaml`:

```yaml
rules:
  standin-events:
    session:
      when:
        type: "session"
      emit: "ready"
      session: "id"
    started:
      when:
        type: "started"
      emit: "turn_started"
    text:
      when:
        type: "text"
      emit: "text"
      text: "delta"
    tool:
      when:
        type: "tool"
      emit: "tool_use"
      name: "name"
```

## Running in a terminal

`terminal.record.reader` names how the engine reads back what the harness did in a live terminal:

| Reader | Reads |
| --- | --- |
| `codex-rollouts` | Codex's own rollout files. |
| `claude-session` | Claude Code's own session files. |
| `jsonl-append` | A JSON lines file the harness appends to, read with a rule set. |
| `json-rewrite` | A JSON file the harness rewrites whole, read with a rule set. |
| `sqlite-poll` | A SQLite database the harness writes, read only, by polling. |
| `markdown-history` | A Markdown history file. |
| `screen` | The terminal's screen text, for a harness with no readable record. It gives no tool names and can cut or repeat text. |
| `unavailable` | Nothing the engine can read. The default. |

For a listed harness, the engine runs the live terminal from the manifest when the reader is `jsonl-append` or `json-rewrite`. Then `terminal` holds only these keys:

| Key | Needed | Meaning |
| --- | --- | --- |
| `argv` | yes | The arguments of a fresh run. With `{session}`, the run takes the engine's session id; without it, the harness picks its own and the engine reads it back from the record. |
| `resume` | no | The arguments of a resumed run. Must hold `{session}`. |
| `submit` | no | The key that sends a typed prompt. Default `enter`. |
| `interrupt` | no | The key that stops the running turn. Default `escape`. |
| `first_screen` | no | At most 4 named steps answered before the first prompt, in file order. Each has `seen`, the screen text to wait for (at most 64 bytes), `press`, the key to press once, and `optional`, `true` for a screen the harness shows only on its first run in a home: pressed when it shows, never waited for. |
| `record` | yes | `reader`, `path` and `rules`. |

Keys come from a fixed list: `enter`, `escape`, `ctrl-c`, `tab`. A manifest never hands the terminal bytes of its own. `{prompt}`, `{relay_url}`, `{token}` and `{key_file}` are not filled in a terminal run.

`record.path` is where the harness writes its record: `{home}/` and then parts that are plain names, names with `*` wildcards, or `**` for any number of folders, at most once. It lies at most 8 levels below the private home and is at most 512 bytes as written. `{session}` is filled with the session id, at most 64 bytes of plain text, or matches any name for a harness that picks its own id. One search of a record path looks at no more than 512 folder entries.

From `standin-json-rewrite.yaml`:

```yaml
terminal:
  argv:
    - "--home"
    - "{home}"
    - "--session"
    - "{session}"
  resume:
    - "--home"
    - "{home}"
    - "--resume"
    - "{session}"
  submit: "enter"
  interrupt: "ctrl-c"
  first_screen:
    trust:
      seen: "Trust this folder?"
      press: "enter"
  record:
    reader: "json-rewrite"
    path: "{home}/standin/records/{session}.json"
    rules: "standin-record"
```

## The import block

Files a harness keeps in the owner's home that CrazyAgent's imports read from. Each is read only, never written, and only where its harness keeps its own files.

| Key | Meaning |
| --- | --- |
| `skills` | The harness's skills folder. The skills import lists it after the Claude Code and Codex skills folders, in catalog order; a folder an earlier source already reads is not listed twice. |
| `instructions` | The harness's own instructions file. |
| `conversations` | A pattern of the harness's past conversation records, read back for the owner's import of past conversations. It needs a record reader that keeps past conversations; `sqlite-poll`, `screen` and `unavailable` do not. |

Each path is relative to the owner's home: no `~`, `.` or `..` part and no leading `/`, at most 240 bytes with no control character. It must lie inside one of the manifest's own `home.owner_folders`, never another harness's folders or anything else in the home. Any other key is refused.

From `codex.yaml`:

```yaml
import:
  conversations: ".codex/sessions/**/rollout-*.jsonl"
```

## The update block

The install and update recipe. RCLI has none: it ships with the app.

From `claude-code.yaml`:

```yaml
update:
  own:
    root: ".local/share/claude/versions"
    args:
      - "update"
    pointer:
      url: "https://downloads.claude.ai/claude-code-releases/latest"
      format: "plain-version"
      bytes_max: 64
  casks:
    - "claude-code"
  known_casks:
    - "claude-code@latest"
  npm:
    scope: "@anthropic-ai"
    name: "claude-code"
    package: "@anthropic-ai/claude-code"
    spec: "@anthropic-ai/claude-code@latest"
  guide: "https://code.claude.com/docs/en/setup"
```

| Key | Needed | Meaning |
| --- | --- | --- |
| `guide` | yes | The vendor's install page, https. |
| `own` | no | The harness updates itself: `root` is the folder under the home folder its versions live in, `args` its own update command (1 to 5 words), `pointer` the vendor's release pointer. |
| `own.pointer` | with `own` | `url` (https), `format` (`plain-version`, or `json-tag` with `field` and `prefix`), `bytes_max` (1 to 262144, default 262144). |
| `formulae`, `casks` | no | Homebrew names the app may install and update. At most one name the app may install per harness. |
| `known_formulae`, `known_casks` | no | Homebrew names the app recognizes and updates but never installs: a deprecated formula or a second channel. |
| `npm` | no | `name`, `package` and `spec` are needed; `scope` (default none) and `flags` (at most 1) are not. |

These values become arguments of install and update commands, so the reader is strict:

- A Homebrew name is one path part: lower-case letters, digits and `@ . _ -`, starting with a letter or a digit. So it cannot be read as an option, a folder or an address.
- An npm name, an npm scope after its `@`, and a version or tag are each one part: lower-case letters, digits and `. _ -`, starting with a letter or a digit, with no `@` inside and not ending in `.tgz`, `.tar` or `.gz`.
- `package` is the block's own `scope`/`name`, or `name` alone when there is no scope. Anything else is refused.
- `spec` is `package` alone, or `package`, `@` and one version or tag. So npm cannot read it as an option, a folder (`cli@.`), an archive (`evil.tgz`), an address (`file:`, `git+`, `http`, `npm:`) or a package other than the one the app checks the version of.
- A flag starts with two dashes and holds lower-case letters, digits, `-` and `=`.
- An argument of the harness's own updater is a word of lower-case letters, digits and dashes.
- Every address is https and has no space.
- `root` is a folder under the home folder: no `..`, no leading slash.
- A key the block does not have is refused, at every level.
- At most 4 Homebrew names in all; a name listed twice is refused.

A refusal is one sentence that names the setting, such as "update.formulae: --force is not a name of lower-case letters, digits and @ . _ - that starts with a letter or a digit."

## Placeholders

The engine fills only these, written in braces: `{home}`, `{session}`, `{model}`, `{prompt}`, `{cwd}`, `{relay_url}`, `{token}`, `{key_file}`.

- `{home}` is the harness's private home, never the owner's.
- `{token}` is a placeholder, never a key.
- Each block fills only the placeholders listed for it above.
- Any other lower-case word in braces is refused, so a manifest cannot ask for a value the engine does not hand out.
- A brace followed by anything else, such as the braces of JSON text, is plain text.

## Refusals

Every refusal has a stable code and one plain sentence that names the setting.

| Code | Example sentence |
| --- | --- |
| `harness_manifest_yaml` | "The file is larger than 16 KiB." or "Line 12: list items are single values; lists of maps are not supported." |
| `harness_manifest_schema` | "schema: this app reads schema: 1 harness manifests only." |
| `harness_manifest_unknown_key` | "colour: this key is not one a harness manifest has." |
| `harness_manifest_missing` | "id: this setting is needed." |
| `harness_manifest_type` | "name: this must be text in quotes." |
| `harness_manifest_value` | "locations: ../etc is not a folder under the home folder." |
| `harness_manifest_kind` | "piped.driver: CrazyAgent has no driver named telepathy." or "limits: CrazyAgent has no limits reader named moonshot." |
| `harness_manifest_placeholder` | "piped.argv: {secret} is not a value CrazyAgent fills in." |
| `harness_manifest_duplicate` | "two: the program one-cli is already listed by one." |

The registry puts the file name in front: "two: schema: this app reads schema: 1 harness manifests only."

A bundled manifest that is refused is a programming error: the registry stops the engine at first use. The row `every_bundled_manifest_loads_in_catalog_order_under_its_own_file_name` guards it.

## Adding a harness

1. Write `crates/crazyagent/harness-manifests/<id>.yaml`.
2. Add one line for it to `BUNDLED` in `crates/crazyagent/src/harnesses/registry.rs`, at its place in catalog order. All 18 ids are already there, so wiring one of them edits only its manifest.
3. Check its blocks against a stand-in in `crates/crazyagent/tests/fixtures/harness/` of the same driver or record reader.
