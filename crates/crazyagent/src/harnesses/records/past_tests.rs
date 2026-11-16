use super::*;
use crate::app_plugins::yaml::Value as Yaml;
use crate::harnesses::manifest;
use crate::harnesses::records::json_files::tests::Home;
use crate::harnesses::registry::registry;
use serde_json::{Value, json};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::time::Duration;

const OLD: Duration = Duration::from_secs(60);
const RULES: &str = "rules:\n  standin-record:\n    asked:\n      when:\n        role: \"user\"\n      emit: \"turn_started\"\n      text: \"content\"\n    said:\n      when:\n        role: \"agent\"\n      emit: \"text\"\n      text: \"content\"\n";
const RECORD_RULES: &str = "    rules: \"standin-record\"\n";

/// A stand-in manifest that keeps its files in `.standin`. Inputs: the record reader, none
/// for no terminal record, the `import` block lines, the extra `terminal.record` lines and
/// the extra top-level blocks. Output: the parsed manifest.
fn manifest_of(reader: &str, import: &str, record: &str, blocks: &str) -> Manifest {
    let terminal = if reader.is_empty() {
        String::new()
    } else {
        format!("terminal:\n  record:\n    reader: \"{reader}\"\n{record}")
    };
    let text = format!(
        "schema: 1\nkind: harness\nid: \"standin-past\"\nname: \"Stand-in past\"\nbinaries:\n  - \"standin-past\"\nheadless: \"standin-past --json\"\nmodel_selection: true\naccount_kinds:\n  - \"api_key\"\nhome:\n  owner_folders:\n    - \".standin\"\n{terminal}{import}{blocks}"
    );
    manifest::parse(text.as_bytes()).unwrap()
}

/// The `import` block lines that name one conversations pattern. Input: the pattern.
/// Output: the lines.
fn conversations(pattern: &str) -> String {
    format!("import:\n  conversations: \"{pattern}\"\n")
}

/// A stand-in manifest whose records the stand-in rules read. Inputs: the record reader and
/// the conversations pattern. Output: the manifest.
fn ruled(reader: &str, pattern: &str) -> Manifest {
    manifest_of(reader, &conversations(pattern), RECORD_RULES, RULES)
}

/// A stand-in manifest whose records are Claude Code sessions. Input: the pattern. Output:
/// the manifest.
fn sessions(pattern: &str) -> Manifest {
    manifest_of("claude-session", &conversations(pattern), "", "")
}

fn lines(items: &[Value]) -> String {
    items.iter().map(|item| format!("{item}\n")).collect()
}

fn claude(kind: &str, content: Value) -> Value {
    json!({"type": kind, "cwd": "/work/app", "message": {"content": content}})
}

fn codex(role: &str, part: &str, text: &str) -> Value {
    json!({
        "type": "response_item",
        "payload": {"type": "message", "role": role, "content": [{"type": part, "text": text}]}
    })
}

fn reply(text: &str) -> Value {
    json!({"role": "agent", "content": text})
}

fn message(ordinal: usize, speaker: Speaker, text: &str) -> Message {
    Message {
        ordinal,
        speaker,
        text: text.into(),
    }
}

fn said(imported: &Imported) -> Vec<Vec<(Speaker, &str)>> {
    imported
        .conversations
        .iter()
        .map(|conversation| {
            conversation
                .messages
                .iter()
                .map(|message| (message.speaker, message.text.as_str()))
                .collect()
        })
        .collect()
}

fn records(imported: &Imported) -> Vec<&str> {
    imported
        .conversations
        .iter()
        .map(|conversation| conversation.record.to_str().unwrap())
        .collect()
}

fn skipped(imported: &Imported) -> Vec<(&str, &'static str)> {
    imported
        .skipped
        .iter()
        .map(|skip| (skip.record.to_str().unwrap(), skip.reason))
        .collect()
}

/// Every entry below one folder with its size, mode and last change, read without following
/// a link, sorted. Input: the folder. Output: the entries.
fn snapshot(folder: &Path) -> Vec<(PathBuf, u64, u32, SystemTime)> {
    let mut seen = Vec::new();
    let mut left = vec![folder.to_path_buf()];
    while let Some(next) = left.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                left.push(path.clone());
            }
            let mode = metadata.permissions().mode();
            seen.push((path, metadata.len(), mode, metadata.modified().unwrap()));
        }
    }
    seen.sort();
    seen
}

/// Sets the mode of a folder, of every folder below it and of every file below it. Inputs:
/// the folder and the two modes.
fn modes(folder: &Path, folders: u32, files: u32) {
    for (path, ..) in snapshot(folder) {
        let mode = if path.is_dir() { folders } else { files };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    std::fs::set_permissions(folder, std::fs::Permissions::from_mode(folders)).unwrap();
}

/// DEC-180 Task 25a: the bundled Claude Code and Codex manifests name their own session
/// layouts inside their owner folders, and an import reads each record's typed messages and
/// replies in order with their speakers, ordinals and working folder; meta, tool and other
/// lines give no message, and each harness reads only its own folders.
#[test]
fn claude_code_and_codex_records_give_their_messages_in_order() {
    let pattern = |id: &str| {
        let manifest = registry().get(id).unwrap().manifest();
        match manifest.block("import").and_then(|import| import.get("conversations")) {
            Some(Yaml::Str(text)) => Some(text.clone()),
            _ => None,
        }
    };
    assert_eq!(
        pattern("claude-code").as_deref(),
        Some(".claude/projects/*/*.jsonl")
    );
    assert_eq!(
        pattern("codex").as_deref(),
        Some(".codex/sessions/**/rollout-*.jsonl")
    );

    let home = Home::new();
    let tool = json!([{"type": "text", "text": "On it."}, {"type": "tool_use", "name": "Bash"}]);
    let session = [
        json!({"type": "summary", "summary": "Build fix"}),
        claude("user", json!("fix the build")),
        json!({"type": "user", "isMeta": true, "cwd": "/work/app", "message": {"content": "m"}}),
        claude("assistant", tool),
        claude("user", json!([{"type": "tool_result", "content": "ok"}])),
        claude("assistant", json!([{"type": "text", "text": "Done."}])),
    ];
    home.write(".claude/projects/-work-app/a1.jsonl", &lines(&session), OLD);
    let rollout = [
        json!({"type": "session_meta", "payload": {"id": "c1", "cwd": "/work/api"}}),
        codex("user", "input_text", "add a test"),
        json!({"type": "response_item", "payload": {"type": "function_call", "name": "shell"}}),
        codex("assistant", "output_text", "Added."),
    ];
    let place = ".codex/sessions/2026/11/15/rollout-2026-11-15T10-00-00-c1.jsonl";
    home.write(place, &lines(&rollout), OLD);
    home.write(
        ".codex/sessions/2026/11/15/notes.txt",
        "not a rollout\n",
        OLD,
    );

    let from_claude = import(
        registry().get("claude-code").unwrap().manifest(),
        &home.path(),
    )
    .unwrap();
    assert_eq!(
        records(&from_claude),
        [".claude/projects/-work-app/a1.jsonl"]
    );
    let conversation = &from_claude.conversations[0];
    assert_eq!(conversation.cwd.as_deref(), Some("/work/app"));
    assert!(!conversation.clipped);
    assert_eq!(
        conversation.messages,
        [
            message(0, Speaker::Input, "fix the build"),
            message(1, Speaker::Agent, "On it."),
            message(2, Speaker::Agent, "Done."),
        ]
    );
    assert!(from_claude.skipped.is_empty() && !from_claude.cut);

    let from_codex = import(registry().get("codex").unwrap().manifest(), &home.path()).unwrap();
    assert_eq!(records(&from_codex), [place]);
    assert_eq!(
        from_codex.conversations[0].cwd.as_deref(),
        Some("/work/api")
    );
    assert_eq!(
        said(&from_codex),
        [vec![(Speaker::Input, "add a test"), (Speaker::Agent, "Added.")]]
    );
    assert!(from_codex.skipped.is_empty() && !from_codex.cut);
}

/// DEC-180 Task 25a: a jsonl-append record's lines and a json-rewrite record's document give
/// messages by the rule set `terminal.record.rules` names: a typed prompt from a
/// `turn_started` rule's text, a reply from a `text` rule; other and empty texts give none.
#[test]
fn jsonl_append_and_json_rewrite_records_give_messages_by_their_record_rules() {
    let home = Home::new();
    let turn = [
        json!({"role": "user", "content": "first"}),
        json!({"role": "tool", "content": "ls"}),
        reply("reply"),
        json!({"role": "user", "content": ""}),
        reply("second"),
    ];
    home.write(".standin/lines/s1.jsonl", &lines(&turn), OLD);
    let document = serde_json::to_string(&turn).unwrap();
    home.write(".standin/docs/s1.json", &document, OLD);
    let expected = [vec![
        (Speaker::Input, "first"),
        (Speaker::Agent, "reply"),
        (Speaker::Agent, "second"),
    ]];

    let appended = import(
        &ruled("jsonl-append", ".standin/lines/*.jsonl"),
        &home.path(),
    )
    .unwrap();
    assert_eq!(said(&appended), expected);
    assert_eq!(appended.conversations[0].cwd, None);
    assert_eq!(appended.conversations[0].messages[2].ordinal, 2);
    let rewritten = import(&ruled("json-rewrite", ".standin/docs/*.json"), &home.path()).unwrap();
    assert_eq!(said(&rewritten), expected);
    assert_eq!(records(&rewritten), [".standin/docs/s1.json"]);
}

/// DEC-180 Task 25a: a Markdown chat history gives each typed prompt, its `#### ` lines
/// joined, and each reply, its lines joined, as one message; session headers, `>` notes and
/// blank lines give none.
#[test]
fn a_markdown_history_gives_each_prompt_and_each_reply_as_one_message() {
    let home = Home::new();
    home.write(
        ".standin/history.md",
        "# aider chat started at 2026-11-15 18:00:00\n\n> Aider v0.80\n\n#### fix the build\n#### and the tests\n\nOn it.\nSecond line.\n\n> Tokens: 1k\n\n# aider chat started at 2026-11-15 19:00:00\n\n#### next\n\nDone.\n",
        OLD,
    );
    let manifest = manifest_of(
        "markdown-history",
        &conversations(".standin/history.md"),
        "",
        "",
    );
    let imported = import(&manifest, &home.path()).unwrap();
    assert_eq!(
        said(&imported),
        [vec![
            (Speaker::Input, "fix the build\nand the tests"),
            (Speaker::Agent, "On it.\nSecond line."),
            (Speaker::Input, "next"),
            (Speaker::Agent, "Done."),
        ]]
    );
}

/// DEC-180 Task 25a, plan row 25 bounds: records past the bound are not read, in path order,
/// and the import says it was cut; so does a search past its folder entries and a read past
/// the bytes in all. A record past its bytes or its messages is read up to the bound and
/// says it was clipped; a line past its bound is passed over, the next is still read, and
/// the record says it was clipped.
#[test]
fn bounds_on_records_bytes_and_messages_cut_the_import_and_say_so() {
    let home = Home::new();
    let manifest = ruled("jsonl-append", ".standin/r/*.jsonl");
    for name in ["c", "a", "b"] {
        let place = format!(".standin/r/{name}.jsonl");
        home.write(&place, &lines(&[reply(name)]), OLD);
    }
    let within = |manifest: &Manifest, bounds: Bounds| {
        import_within(manifest, &home.path(), &bounds).unwrap()
    };

    let all = within(&manifest, Bounds::DEFAULT);
    assert_eq!(
        records(&all),
        [".standin/r/a.jsonl", ".standin/r/b.jsonl", ".standin/r/c.jsonl"]
    );
    assert!(!all.cut);
    let two = within(
        &manifest,
        Bounds {
            records: 2,
            ..Bounds::DEFAULT
        },
    );
    assert_eq!(records(&two), [".standin/r/a.jsonl", ".standin/r/b.jsonl"]);
    assert!(two.cut);
    let scanned = within(
        &manifest,
        Bounds {
            entries: 3,
            ..Bounds::DEFAULT
        },
    );
    assert!(scanned.cut);
    assert!(scanned.conversations.len() <= 1);
    let one_line = lines(&[reply("a")]).len() as u64;
    let total = within(
        &manifest,
        Bounds {
            total_bytes: one_line,
            ..Bounds::DEFAULT
        },
    );
    assert_eq!(records(&total), [".standin/r/a.jsonl"]);
    assert!(total.cut);
    assert!(!total.conversations[0].clipped);

    let long = ruled("jsonl-append", ".standin/m/*.jsonl");
    let three = [reply("one"), reply("two"), reply("three")];
    home.write(".standin/m/long.jsonl", &lines(&three), OLD);
    let few = within(
        &long,
        Bounds {
            messages: 2,
            ..Bounds::DEFAULT
        },
    );
    assert_eq!(
        said(&few),
        [vec![(Speaker::Agent, "one"), (Speaker::Agent, "two")]]
    );
    assert!(few.conversations[0].clipped && !few.cut);
    let first = lines(&[reply("one")]).len() as u64;
    let short = within(
        &long,
        Bounds {
            record_bytes: first,
            ..Bounds::DEFAULT
        },
    );
    assert_eq!(said(&short), [vec![(Speaker::Agent, "one")]]);
    assert!(short.conversations[0].clipped);

    let over = ruled("jsonl-append", ".standin/o/*.jsonl");
    let middle = reply(&"x".repeat(64));
    home.write(
        ".standin/o/s.jsonl",
        &lines(&[reply("before"), middle, reply("after")]),
        OLD,
    );
    let passed = within(
        &over,
        Bounds {
            line_bytes: 48,
            ..Bounds::DEFAULT
        },
    );
    assert_eq!(
        said(&passed),
        [vec![(Speaker::Agent, "before"), (Speaker::Agent, "after")]]
    );
    assert!(passed.conversations[0].clipped);
}

/// DEC-180 Task 25a, plan row 25: a record that cannot be opened, holds nothing readable,
/// holds no message or is a document past its bound is skipped with a plain reason, while a
/// record with some readable lines keeps them and every other record is still read.
#[test]
fn a_broken_record_is_skipped_with_a_plain_reason_and_the_rest_still_read() {
    let home = Home::new();
    let good = lines(&[claude("user", json!("hi"))]);
    home.write(".standin/s/a-good.jsonl", &good, OLD);
    home.write(".standin/s/b-not-json.jsonl", "not json\n{broken\n", OLD);
    home.write(".standin/s/c-empty.jsonl", "", OLD);
    let binary = home.write(".standin/s/d-binary.jsonl", "", OLD);
    std::fs::write(&binary, [0xff, 0xfe, b'\n']).unwrap();
    home.write(".standin/s/e-partial.jsonl", &format!("{good}{{trunc"), OLD);
    let locked = home.write(".standin/s/f-locked.jsonl", &good, OLD);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

    let imported = import(&sessions(".standin/s/*.jsonl"), &home.path()).unwrap();
    assert_eq!(
        records(&imported),
        [".standin/s/a-good.jsonl", ".standin/s/e-partial.jsonl"]
    );
    assert_eq!(said(&imported)[1], [(Speaker::Input, "hi")]);
    assert_eq!(
        skipped(&imported),
        [
            (".standin/s/b-not-json.jsonl", BROKEN),
            (".standin/s/c-empty.jsonl", EMPTY),
            (".standin/s/d-binary.jsonl", BROKEN),
            (".standin/s/f-locked.jsonl", UNREADABLE),
        ]
    );
    assert!(!imported.cut);

    let documents = ruled("json-rewrite", ".standin/d/*.json");
    let big = serde_json::to_string(&[reply("long enough to pass the bound")]).unwrap();
    home.write(".standin/d/a-big.json", &big, OLD);
    home.write(".standin/d/b-object.json", &reply("one").to_string(), OLD);
    home.write(".standin/d/c-not-json.json", "[{", OLD);
    home.write(
        ".standin/d/d-small.json",
        "[{\"role\":\"agent\",\"content\":\"s\"}]",
        OLD,
    );
    let bounds = Bounds {
        record_bytes: 40,
        ..Bounds::DEFAULT
    };
    let read = import_within(&documents, &home.path(), &bounds).unwrap();
    assert_eq!(said(&read), [vec![(Speaker::Agent, "s")]]);
    assert_eq!(
        skipped(&read),
        [
            (".standin/d/a-big.json", TOO_LARGE),
            (".standin/d/b-object.json", BROKEN),
            (".standin/d/c-not-json.json", BROKEN),
        ]
    );

    let history = home.write(".standin/h/history.md", "", OLD);
    std::fs::write(&history, [b'#', b'#', b'#', b'#', b' ', 0xff, b'\n']).unwrap();
    let markdown = manifest_of(
        "markdown-history",
        &conversations(".standin/h/*.md"),
        "",
        "",
    );
    let unreadable = import(&markdown, &home.path()).unwrap();
    assert_eq!(skipped(&unreadable), [(".standin/h/history.md", BROKEN)]);

    for code in [UNREADABLE, TOO_LARGE, BROKEN, EMPTY] {
        let plain = sentence(code).unwrap();
        assert!(plain.ends_with('.') && !plain.contains("import_"), "{code}");
    }
    assert_eq!(sentence("import_record_other"), None);
}

/// DEC-180 Task 25a, plan row 25: the owner's folders are only read. An import of a tree
/// the owner made read-only reads every record, and afterwards every entry has the same
/// name, size, mode and last change.
#[test]
fn an_import_only_reads_the_owner_folders_and_changes_nothing_in_them() {
    let home = Home::new();
    let owner = home.path().join(".standin");
    for name in ["a", "b"] {
        let record = lines(&[claude("user", json!(name))]);
        home.write(&format!(".standin/s/{name}.jsonl"), &record, OLD);
    }
    modes(&owner, 0o500, 0o400);
    let before = snapshot(&owner);

    let imported = import(&sessions(".standin/s/*.jsonl"), &home.path());

    let after = snapshot(&owner);
    modes(&owner, 0o700, 0o600);
    let imported = imported.unwrap();
    assert_eq!(
        said(&imported),
        [vec![(Speaker::Input, "a")], vec![(Speaker::Input, "b")]]
    );
    assert!(imported.skipped.is_empty());
    assert_eq!(before.len(), 3);
    assert_eq!(before, after);
}

/// DEC-180 Task 25a: a link below the owner's folders, to a record or to a folder, is never
/// listed or followed, and neither is an owner folder that is itself a link.
#[test]
fn a_link_below_the_owner_folders_is_never_followed() {
    let home = Home::new();
    let secret = lines(&[claude("user", json!("secret"))]);
    let outside = home.write("elsewhere/s/x.jsonl", &secret, OLD);
    let kept = lines(&[claude("user", json!("kept"))]);
    home.write(".standin/s/real.jsonl", &kept, OLD);
    symlink(&outside, home.path().join(".standin/s/link.jsonl")).unwrap();
    symlink(
        outside.parent().unwrap(),
        home.path().join(".standin/linked"),
    )
    .unwrap();

    let imported = import(&sessions(".standin/**/*.jsonl"), &home.path()).unwrap();
    assert_eq!(records(&imported), [".standin/s/real.jsonl"]);
    assert!(imported.skipped.is_empty());

    let linked = Home::new();
    linked.write("elsewhere/s/x.jsonl", &secret, OLD);
    let elsewhere = linked.path().join("elsewhere");
    symlink(elsewhere, linked.path().join(".standin")).unwrap();
    let none = import(&sessions(".standin/**/*.jsonl"), &linked.path()).unwrap();
    assert_eq!(none, Imported::default());
}

/// DEC-180 Task 25a: each refused import setting names its setting in one plain sentence:
/// a pattern outside the owner folders or not written below the home, a misplaced `**`, a
/// placeholder, too many parts, a record kind with no past conversations to read back, and
/// a missing or refused `terminal.record.rules`. No `import.conversations` reads nothing.
#[test]
fn each_refused_import_setting_names_its_setting() {
    let home = Home::new();
    let refused = |manifest: &Manifest| {
        import(manifest, &home.path())
            .err()
            .map(|refusal| (refusal.code, refusal.message))
    };
    let pattern = |text: &str| refused(&sessions(text));
    let setting = |code: &'static str, problem: &str| {
        Some((code, format!("import.conversations: {problem}.")))
    };
    let not_under_home = setting(
        manifest::VALUE,
        "this must be a pattern under the home folder, written without ~, . or .. parts or a \
         leading /",
    );
    let cases = [
        (
            ".other/*.jsonl",
            setting(
                manifest::VALUE,
                "this must be inside one of home.owner_folders",
            ),
        ),
        ("/home/mira/.standin/*.jsonl", not_under_home.clone()),
        (".standin/../.ssh/*", not_under_home),
        (
            ".standin/**/a/**/*.jsonl",
            setting(
                manifest::VALUE,
                "** may stand once, and never for the file itself",
            ),
        ),
        (
            ".standin/**",
            setting(
                manifest::VALUE,
                "** may stand once, and never for the file itself",
            ),
        ),
        (
            ".standin/a**b/*.jsonl",
            setting(manifest::VALUE, "** must be a whole part"),
        ),
        (
            ".standin/{session}.jsonl",
            setting(manifest::VALUE, "nothing is filled in here"),
        ),
        (
            ".standin/a/b/c/d/e/f/g/*.jsonl",
            setting(
                manifest::VALUE,
                "this must be at most 8 parts below the home folder",
            ),
        ),
    ];
    for (text, refusal) in cases {
        assert_eq!(pattern(text), refusal, "{text}");
    }
    assert_eq!(
        refused(&manifest_of(
            "claude-session",
            "import:\n  conversations:\n    - \".standin/*.jsonl\"\n",
            "",
            ""
        )),
        setting(manifest::TYPE, "this must be text in quotes")
    );

    let kind = setting(
        manifest::KIND,
        "this harness's terminal record keeps no past conversations CrazyAgent reads back",
    );
    let valid = conversations(".standin/*.jsonl");
    for reader in ["screen", "sqlite-poll", ""] {
        assert_eq!(
            refused(&manifest_of(reader, &valid, "", "")),
            kind,
            "{reader}"
        );
    }
    let rules = |record: &str, blocks: &str| {
        refused(&manifest_of("jsonl-append", &valid, record, blocks))
    };
    assert_eq!(
        rules("", RULES),
        Some((
            manifest::MISSING,
            "terminal.record.rules: this setting is needed to import past conversations."
                .to_owned()
        ))
    );
    assert_eq!(
        rules("    rules: 3\n", RULES),
        Some((
            manifest::TYPE,
            "terminal.record.rules: this must be text in quotes.".to_owned()
        ))
    );
    assert_eq!(
        rules(RECORD_RULES, ""),
        Some((
            manifest::MISSING,
            "rules.standin-record: this setting is needed.".to_owned()
        ))
    );

    let quiet = manifest_of("claude-session", "", "", "");
    assert_eq!(import(&quiet, &home.path()), Ok(Imported::default()));
    let empty = manifest_of("screen", "import:\n  conversations: null\n", "", "");
    assert_eq!(import(&empty, &home.path()), Ok(Imported::default()));
}

/// rv-sa241 RULING-19:21-t20-q1 residual: a relay token a tool printed into a conversation,
/// `crazyagent-relay-` and 64 lowercase hex digits, never reaches the record set, whatever
/// follows it; a shorter token, an upper-case one and a bare prefix stay as written.
#[test]
fn a_relay_token_in_an_imported_message_is_redacted() {
    let home = Home::new();
    let token = format!("crazyagent-relay-{}", "a1".repeat(32));
    let short = format!("crazyagent-relay-{}", "a".repeat(63));
    let upper = format!("crazyagent-relay-{}", "A".repeat(64));
    let typed = format!("KEY={token} and {token}f; {short}; {upper}; crazyagent-relay-");
    let session = [
        claude("user", json!(typed)),
        claude("assistant", json!(format!("saw {token}"))),
    ];
    home.write(".standin/s/a.jsonl", &lines(&session), OLD);

    let imported = import(&sessions(".standin/s/*.jsonl"), &home.path()).unwrap();
    let kept = format!(
        "KEY={RELAY_REDACTED} and {RELAY_REDACTED}f; {short}; {upper}; crazyagent-relay-"
    );
    let reply = format!("saw {RELAY_REDACTED}");
    assert_eq!(
        said(&imported),
        [vec![
            (Speaker::Input, kept.as_str()),
            (Speaker::Agent, reply.as_str()),
        ]]
    );
}

/// DEC-180 Task 25a: a relay token is redacted before a record rule clips the text that
/// holds it, so a token at the clip edge keeps none of its hex digits, in a jsonl-append
/// line and in a json-rewrite document alike.
#[test]
fn a_relay_token_at_a_record_rule_clip_edge_keeps_none_of_its_digits() {
    let home = Home::new();
    let token = format!("crazyagent-relay-{}", "a1".repeat(32));
    let content = format!("{token} ").repeat(800);
    assert!(content.len() > crate::adapters::FIELD_BYTES_MAX);
    let turn = [json!({"role": "user", "content": content})];
    home.write(".standin/lines/s1.jsonl", &lines(&turn), OLD);
    let document = serde_json::to_string(&turn).unwrap();
    home.write(".standin/docs/s1.json", &document, OLD);

    let appended = import(
        &ruled("jsonl-append", ".standin/lines/*.jsonl"),
        &home.path(),
    )
    .unwrap();
    let rewritten = import(&ruled("json-rewrite", ".standin/docs/*.json"), &home.path()).unwrap();
    for imported in [&appended, &rewritten] {
        let kept = &imported.conversations[0].messages[0].text;
        assert!(kept.contains(RELAY_REDACTED));
        for (at, _) in kept.match_indices(TOKEN_PREFIX) {
            let after = &kept[at + TOKEN_PREFIX.len()..];
            assert!(!after.starts_with(|c: char| c.is_ascii_hexdigit()));
        }
    }
}

/// DEC-180 Task 25a: a record read up to its byte bound never keeps the line the bound cut,
/// so a relay token at that edge keeps none of its hex digits; the lines before it are kept
/// and the conversation says it was clipped.
#[test]
fn a_line_cut_by_the_byte_bound_is_not_kept_so_no_relay_token_is_half_kept() {
    let home = Home::new();
    let token = format!("crazyagent-relay-{}", "a1".repeat(32));
    let kept = "#### first\n\nreply one\n";
    let history = format!("{kept}#### key {token}\n");
    home.write(".standin/cut.md", &history, OLD);
    let manifest = manifest_of(
        "markdown-history",
        &conversations(".standin/cut.md"),
        "",
        "",
    );
    let edge = format!("{kept}#### key crazyagent-relay-a1a1a").len() as u64;

    let imported = import_within(
        &manifest,
        &home.path(),
        &Bounds {
            record_bytes: edge,
            ..Bounds::DEFAULT
        },
    )
    .unwrap();
    assert_eq!(
        said(&imported),
        [vec![(Speaker::Input, "first"), (Speaker::Agent, "reply one")]]
    );
    assert!(imported.conversations[0].clipped);
}
