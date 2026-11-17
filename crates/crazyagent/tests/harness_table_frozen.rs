use crazyagent::harnesses::{AccountKind, HarnessSpec, specs};

const NO_ADAPTER: &str = "installed, but CrazyAgent cannot start it yet";
const ALL_ACCOUNTS: &[AccountKind] = &[
    AccountKind::Subscription,
    AccountKind::ApiKey,
    AccountKind::OpenAiCompatibleUrl,
    AccountKind::Local,
];
const KEY_URL_LOCAL: &[AccountKind] = &[
    AccountKind::ApiKey,
    AccountKind::OpenAiCompatibleUrl,
    AccountKind::Local,
];

/// The harness table as it stood at dd2f885de, the commit before the manifests replaced it:
/// every field of all 18 rows, in catalog order.
const FROZEN: [HarnessSpec; 18] = [
    HarnessSpec {
        id: "codex",
        name: "Codex",
        binaries: &["codex"],
        locations: &[],
        headless: "codex app-server: JSON-RPC over stdio",
        tools_limited: None,
        model_selection: true,
        account_kinds: ALL_ACCOUNTS,
        launch: None,
    },
    HarnessSpec {
        id: "claude-code",
        name: "Claude Code",
        binaries: &["claude"],
        locations: &[".claude/local"],
        headless: "claude --print with stream-json input and output",
        tools_limited: None,
        model_selection: true,
        account_kinds: &[AccountKind::Subscription, AccountKind::ApiKey],
        launch: None,
    },
    HarnessSpec {
        id: "opencode",
        name: "OpenCode",
        binaries: &["opencode"],
        locations: &[".opencode/bin"],
        headless: "opencode run --format json, or opencode serve over HTTP",
        tools_limited: None,
        model_selection: true,
        account_kinds: ALL_ACCOUNTS,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "gemini-cli",
        name: "Gemini CLI",
        binaries: &["gemini"],
        locations: &[],
        headless: "gemini --prompt with --output-format stream-json",
        tools_limited: None,
        model_selection: true,
        account_kinds: &[AccountKind::Subscription, AccountKind::ApiKey],
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "fx",
        name: "fx by Vercel",
        binaries: &["fx"],
        locations: &[".fx/bin"],
        headless: "fx ask --json: one request, JSON result on stdout",
        tools_limited: Some("no documented switch to turn tools off"),
        model_selection: true,
        account_kinds: KEY_URL_LOCAL,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "openclaw",
        name: "OpenClaw",
        binaries: &["openclaw"],
        locations: &[],
        headless: "openclaw agent --message through its gateway",
        tools_limited: Some("tools are set in OpenClaw's own configuration"),
        model_selection: true,
        account_kinds: &[
            AccountKind::Subscription,
            AccountKind::ApiKey,
            AccountKind::Local,
        ],
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "pi",
        name: "Pi",
        binaries: &["pi"],
        locations: &[],
        headless: "pi --mode json or --mode rpc over stdio",
        tools_limited: None,
        model_selection: true,
        account_kinds: ALL_ACCOUNTS,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "rcli",
        name: "RCLI",
        binaries: &["crazyagent-rcli"],
        locations: &[],
        headless: "CrazyAgent's bundled RCLI host",
        tools_limited: Some("runs without tools"),
        model_selection: true,
        account_kinds: KEY_URL_LOCAL,
        launch: Some("runs only as a CrazyAgent master"),
    },
    HarnessSpec {
        id: "goose",
        name: "Goose",
        binaries: &["goose"],
        locations: &[],
        headless: "goose run --text with --output-format stream-json",
        tools_limited: None,
        model_selection: true,
        account_kinds: KEY_URL_LOCAL,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "aider",
        name: "Aider",
        binaries: &["aider"],
        locations: &[],
        headless: "aider --message: one prompt, plain text reply",
        tools_limited: Some("file edits cannot be switched off apart from a dry run"),
        model_selection: true,
        account_kinds: KEY_URL_LOCAL,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "qwen-code",
        name: "Qwen Code",
        binaries: &["qwen"],
        locations: &[],
        headless: "qwen --prompt with --output-format stream-json",
        tools_limited: None,
        model_selection: true,
        account_kinds: ALL_ACCOUNTS,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "crush",
        name: "Crush",
        binaries: &["crush"],
        locations: &[],
        headless: "crush run: one prompt, plain text reply",
        tools_limited: None,
        model_selection: true,
        account_kinds: KEY_URL_LOCAL,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "droid",
        name: "Factory Droid",
        binaries: &["droid"],
        locations: &[".factory/bin"],
        headless: "droid exec with --output-format stream-json",
        tools_limited: None,
        model_selection: true,
        account_kinds: &[
            AccountKind::Subscription,
            AccountKind::ApiKey,
            AccountKind::OpenAiCompatibleUrl,
        ],
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "copilot",
        name: "GitHub Copilot CLI",
        binaries: &["copilot"],
        locations: &[],
        headless: "copilot --prompt: one prompt, plain text reply",
        tools_limited: None,
        model_selection: true,
        account_kinds: &[AccountKind::Subscription],
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "cline",
        name: "Cline CLI",
        binaries: &["cline"],
        locations: &[],
        headless: "cline with --json output",
        tools_limited: None,
        model_selection: true,
        account_kinds: ALL_ACCOUNTS,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "continue",
        name: "Continue CLI",
        binaries: &["cn"],
        locations: &[],
        headless: "cn --print: one prompt, plain text reply",
        tools_limited: None,
        model_selection: true,
        account_kinds: KEY_URL_LOCAL,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "openhands",
        name: "OpenHands CLI",
        binaries: &["openhands"],
        locations: &[],
        headless: "openhands --headless with a task",
        tools_limited: Some("headless mode always runs its tools"),
        model_selection: true,
        account_kinds: KEY_URL_LOCAL,
        launch: Some(NO_ADAPTER),
    },
    HarnessSpec {
        id: "kimi",
        name: "Kimi CLI",
        binaries: &["kimi"],
        locations: &[],
        headless: "kimi --print with stream-json output",
        tools_limited: None,
        model_selection: true,
        account_kinds: &[AccountKind::Subscription, AccountKind::ApiKey],
        launch: Some(NO_ADAPTER),
    },
];

#[test]
fn the_harness_table_reads_field_for_field_as_it_did_before_the_manifests() {
    let listed = specs();
    assert_eq!(listed.len(), FROZEN.len());
    for (now, then) in listed.iter().zip(FROZEN.iter()) {
        assert_eq!(now.id, then.id);
        assert_eq!(now.name, then.name, "{}", then.id);
        assert_eq!(now.binaries, then.binaries, "{}", then.id);
        assert_eq!(now.locations, then.locations, "{}", then.id);
        assert_eq!(now.headless, then.headless, "{}", then.id);
        assert_eq!(now.tools_limited, then.tools_limited, "{}", then.id);
        assert_eq!(now.model_selection, then.model_selection, "{}", then.id);
        assert_eq!(now.account_kinds, then.account_kinds, "{}", then.id);
        assert_eq!(now.launch, then.launch, "{}", then.id);
    }
}
