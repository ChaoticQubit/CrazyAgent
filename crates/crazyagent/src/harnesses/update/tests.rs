use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

struct Machine {
    root: tempfile::TempDir,
}

impl Machine {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap())
                .unwrap(),
        }
    }
    fn roots(&self) -> Roots {
        Roots {
            home: self.root.path().join("home"),
            brew_prefixes: vec![self.root.path().join("brew")],
            casks: true,
        }
    }
    fn program(&self, relative: &str, mode: u32) -> PathBuf {
        let path = self.root.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }
    fn npm(&self, prefix: &str) -> PathBuf {
        std::fs::create_dir_all(self.root.path().join(prefix).join("lib/node_modules")).unwrap();
        self.program(&format!("{prefix}/bin/npm"), 0o755)
    }
}

#[test]
fn claude_from_the_latest_cask_updates_with_that_prefixs_brew_and_that_cask() {
    let machine = Machine::new();
    let brew = machine.program("brew/bin/brew", 0o755);
    let real = machine.program("brew/Caskroom/claude-code@latest/2.1.322/claude", 0o755);
    assert_eq!(
        method("claude-code", &real, &machine.roots()),
        Method::Homebrew {
            cask: "claude-code@latest".into()
        }
    );
    let fixed =
        update_command("claude-code", Path::new("/unused"), &real, &machine.roots()).unwrap();
    assert_eq!(fixed.program, brew);
    assert_eq!(fixed.args, ["upgrade", "--cask", "claude-code@latest"]);
    assert_eq!(
        shown(&fixed),
        format!("{} upgrade --cask claude-code@latest", brew.display())
    );
    let check = check_command("claude-code", &real, &machine.roots())
        .unwrap()
        .unwrap();
    assert_eq!(
        check.args,
        [
            "outdated",
            "--cask",
            "--greedy-auto-updates",
            "--json=v2",
            "claude-code@latest"
        ]
    );
}

#[test]
fn codex_from_its_cask_updates_with_the_documented_cask_flag() {
    let machine = Machine::new();
    machine.program("brew/bin/brew", 0o755);
    let real = machine.program("brew/Caskroom/codex/0.167.1/bin/codex", 0o755);
    let fixed = update_command("codex", Path::new("/unused"), &real, &machine.roots()).unwrap();
    assert_eq!(fixed.args, ["upgrade", "--cask", "codex"]);
}

#[test]
fn a_cask_the_table_does_not_name_is_an_unknown_install() {
    let machine = Machine::new();
    machine.program("brew/bin/brew", 0o755);
    let real = machine.program("brew/Caskroom/claude-code-nightly/9.9.9/claude", 0o755);
    assert_eq!(
        method("claude-code", &real, &machine.roots()),
        Method::Unknown
    );
    assert_eq!(
        update_command("claude-code", Path::new("/unused"), &real, &machine.roots()).unwrap_err(),
        UNKNOWN_INSTALL
    );
}

#[test]
fn a_native_claude_install_updates_itself_through_its_own_launcher() {
    let machine = Machine::new();
    let real = machine.program("home/.local/share/claude/versions/2.1.322", 0o755);
    let entry = machine.root.path().join("home/.local/bin/claude");
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &entry).unwrap();
    assert_eq!(
        method("claude-code", &real, &machine.roots()),
        Method::SelfUpdate
    );
    let fixed = update_command("claude-code", &entry, &real, &machine.roots()).unwrap();
    assert_eq!(fixed.program, entry);
    assert_eq!(fixed.args, ["update"]);
    assert!(
        check_command("claude-code", &real, &machine.roots())
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_standalone_codex_install_updates_itself_and_the_installer_script_is_never_a_command() {
    let machine = Machine::new();
    let real = machine.program("home/.codex/packages/standalone/0.167.1/codex", 0o755);
    let entry = machine.root.path().join("home/.local/bin/codex");
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &entry).unwrap();
    let fixed = update_command("codex", &entry, &real, &machine.roots()).unwrap();
    assert_eq!(fixed.program, entry);
    assert_eq!(fixed.args, ["update"]);
    assert!(!shown(&fixed).contains("install.sh"));
}

#[test]
fn an_npm_install_updates_with_the_npm_beside_it_and_only_that_folder_on_the_search_path() {
    let machine = Machine::new();
    let npm = machine.program("node/bin/npm", 0o755);
    let real = machine.program(
        "node/lib/node_modules/@anthropic-ai/claude-code-darwin-arm64/claude",
        0o755,
    );
    assert_eq!(
        method("claude-code", &real, &machine.roots()),
        Method::Npm {
            package: "@anthropic-ai/claude-code".into()
        }
    );
    let fixed =
        update_command("claude-code", Path::new("/unused"), &real, &machine.roots()).unwrap();
    assert_eq!(fixed.program, npm);
    assert_eq!(
        fixed.args,
        ["install", "-g", "@anthropic-ai/claude-code@latest"]
    );
    assert_eq!(fixed.search, vec![machine.root.path().join("node/bin")]);
    let check = check_command("claude-code", &real, &machine.roots())
        .unwrap()
        .unwrap();
    assert_eq!(check.args, ["view", "@anthropic-ai/claude-code", "version"]);
}

#[test]
fn a_package_of_another_scope_with_a_similar_name_is_an_unknown_install() {
    let machine = Machine::new();
    machine.program("node/bin/npm", 0o755);
    let real = machine.program("node/lib/node_modules/@evil/claude-code/claude", 0o755);
    assert_eq!(
        method("claude-code", &real, &machine.roots()),
        Method::Unknown
    );
}

#[test]
fn a_system_package_an_unlisted_harness_and_rcli_are_refused_with_their_own_codes() {
    let machine = Machine::new();
    assert_eq!(
        update_command(
            "claude-code",
            Path::new("/usr/bin/claude"),
            Path::new("/usr/bin/claude"),
            &machine.roots()
        )
        .unwrap_err(),
        UNKNOWN_INSTALL
    );
    machine.program("brew/bin/brew", 0o755);
    let viewer = machine.program("brew/Cellar/fx/40.0.1/bin/fx", 0o755);
    assert_eq!(method("fx", &viewer, &machine.roots()), Method::Unknown);
    assert_eq!(
        update_command("fx", &viewer, &viewer, &machine.roots()).unwrap_err(),
        UNKNOWN_INSTALL
    );
    let wrong_kind = machine.program("brew/Caskroom/aider/1.0/aider", 0o755);
    assert_eq!(
        update_command("aider", &wrong_kind, &wrong_kind, &machine.roots()).unwrap_err(),
        UNKNOWN_INSTALL
    );
    assert_eq!(
        method(
            "rcli",
            Path::new("/Applications/CrazyAgent.app/Contents/MacOS/crazyagent-rcli"),
            &machine.roots()
        ),
        Method::WithApp
    );
    assert_eq!(
        update_command("rcli", Path::new("/x"), Path::new("/x"), &machine.roots()).unwrap_err(),
        WITH_APP
    );
}

#[test]
fn a_missing_or_group_or_world_writable_tool_is_refused() {
    let machine = Machine::new();
    let real = machine.program("brew/Caskroom/codex/0.167.1/bin/codex", 0o755);
    assert_eq!(
        update_command("codex", &real, &real, &machine.roots()).unwrap_err(),
        TOOL_MISSING
    );
    machine.program("brew/bin/brew", 0o777);
    assert_eq!(
        update_command("codex", &real, &real, &machine.roots()).unwrap_err(),
        TOOL_UNSAFE
    );
}

#[test]
fn a_brew_that_is_a_link_to_a_safe_file_is_accepted() {
    let machine = Machine::new();
    let store = machine.program("store/brew", 0o555);
    std::fs::create_dir_all(machine.root.path().join("brew/bin")).unwrap();
    std::os::unix::fs::symlink(&store, machine.root.path().join("brew/bin/brew")).unwrap();
    let real = machine.program("brew/Caskroom/codex/0.167.1/bin/codex", 0o755);
    assert!(update_command("codex", &real, &real, &machine.roots()).is_ok());
}

#[test]
fn every_fixed_command_has_an_absolute_program_and_only_table_arguments() {
    let machine = Machine::new();
    machine.program("brew/bin/brew", 0o755);
    machine.program("node/bin/npm", 0o755);
    for (id, relative) in [
        ("claude-code", "brew/Caskroom/claude-code/2.1.311/claude"),
        ("codex", "node/lib/node_modules/@openai/codex/bin/codex.js"),
    ] {
        let real = machine.program(relative, 0o755);
        let fixed = update_command(id, &real, &real, &machine.roots()).unwrap();
        assert!(fixed.program.is_absolute());
        assert!(fixed.args.len() <= ARGS_MAX);
        assert!(fixed.search.iter().all(|folder| folder.is_absolute()));
    }
}

#[test]
fn the_installed_version_is_the_first_dotted_number_of_the_version_line() {
    assert_eq!(
        installed_version("2.1.322 (Claude Code)\n").as_deref(),
        Some("2.1.322")
    );
    assert_eq!(
        installed_version("codex-cli 0.167.1\n").as_deref(),
        Some("0.167.1")
    );
    assert_eq!(installed_version("no version here"), None);
    assert_eq!(installed_version(&"9".repeat(VERSION_BYTES_MAX + 1)), None);
}

#[test]
fn brew_outdated_json_names_the_newer_version_or_none() {
    let cask = Method::Homebrew {
        cask: "claude-code@latest".into(),
    };
    let outdated = r#"{"formulae":[],"casks":[{"name":"claude-code@latest","installed_versions":["2.1.309"],"current_version":"2.1.322"}]}"#;
    assert_eq!(
        latest(&cask, outdated, "2.1.309"),
        Latest::Newer {
            version: "2.1.322".into()
        }
    );
    assert_eq!(
        latest(&cask, r#"{"formulae":[],"casks":[]}"#, "2.1.322"),
        Latest::UpToDate
    );
    assert_eq!(
        latest(&cask, "not json", "2.1.322"),
        Latest::Failed {
            code: "harness_update_check_unreadable".into()
        }
    );
    let other = r#"{"formulae":[],"casks":[{"name":"codex","installed_versions":["0.1"],"current_version":"0.2"}]}"#;
    assert_eq!(latest(&cask, other, "2.1.322"), Latest::UpToDate);
}

#[test]
fn npm_view_is_newer_only_when_its_version_is_higher_number_by_number() {
    let npm = Method::Npm {
        package: "@openai/codex".into(),
    };
    assert_eq!(
        latest(&npm, "0.167.1\n", "0.167.0"),
        Latest::Newer {
            version: "0.167.1".into()
        }
    );
    assert_eq!(latest(&npm, "0.167.1\n", "0.167.1"), Latest::UpToDate);
    assert_eq!(latest(&npm, "0.99.0\n", "0.167.1"), Latest::UpToDate);
    assert_eq!(
        latest(&npm, "<html>", "0.167.1"),
        Latest::Failed {
            code: "harness_update_check_unreadable".into()
        }
    );
}

#[test]
fn any_catalog_harness_installed_as_a_homebrew_formula_updates_and_checks_by_that_formula() {
    let machine = Machine::new();
    let brew = machine.program("brew/bin/brew", 0o755);
    for (id, formula, relative) in [
        ("aider", "aider", "brew/Cellar/aider/0.86.2/bin/aider"),
        (
            "goose",
            "block-goose-cli",
            "brew/Cellar/block-goose-cli/1.59.0/bin/goose",
        ),
        (
            "gemini-cli",
            "gemini-cli",
            "brew/Cellar/gemini-cli/0.67.0/bin/gemini",
        ),
        ("kimi", "kimi-cli", "brew/Cellar/kimi-cli/1.50.0/bin/kimi"),
    ] {
        let real = machine.program(relative, 0o755);
        assert_eq!(
            method(id, &real, &machine.roots()),
            Method::HomebrewFormula {
                formula: formula.into()
            },
            "{id}"
        );
        let fixed = update_command(id, &real, &real, &machine.roots()).unwrap();
        assert_eq!(fixed.program, brew);
        assert_eq!(fixed.args, ["upgrade", "--formula", formula], "{id}");
        assert_eq!(
            check_command(id, &real, &machine.roots())
                .unwrap()
                .unwrap()
                .args,
            ["outdated", "--formula", "--json=v2", formula],
            "{id}"
        );
    }
}

#[test]
fn a_formula_that_wraps_an_npm_package_is_a_homebrew_install_not_an_npm_one() {
    let machine = Machine::new();
    machine.program("brew/bin/brew", 0o755);
    let real = machine.program(
        "brew/Cellar/qwen-code/0.27.1/libexec/lib/node_modules/@qwen-code/qwen-code/cli.js",
        0o755,
    );
    assert_eq!(
        method("qwen-code", &real, &machine.roots()),
        Method::HomebrewFormula {
            formula: "qwen-code".into()
        }
    );
}

#[test]
fn a_cask_or_npm_install_of_any_catalog_harness_updates_by_its_table_name() {
    let machine = Machine::new();
    machine.program("brew/bin/brew", 0o755);
    machine.program("node/bin/npm", 0o755);
    let droid = machine.program("brew/Caskroom/droid/1.7.1/droid", 0o755);
    assert_eq!(
        update_command("droid", &droid, &droid, &machine.roots())
            .unwrap()
            .args,
        ["upgrade", "--cask", "droid"]
    );
    let prerelease = machine.program(
        "brew/Caskroom/copilot-cli@prerelease/1.0.110-1/copilot",
        0o755,
    );
    assert_eq!(
        update_command("copilot", &prerelease, &prerelease, &machine.roots())
            .unwrap()
            .args,
        ["upgrade", "--cask", "copilot-cli@prerelease"]
    );
    let pi = machine.program(
        "node/lib/node_modules/@earendil-works/pi-coding-agent/dist/cli.js",
        0o755,
    );
    assert_eq!(
        update_command("pi", &pi, &pi, &machine.roots())
            .unwrap()
            .args,
        [
            "install",
            "-g",
            "--ignore-scripts",
            "@earendil-works/pi-coding-agent"
        ]
    );
    let cline = machine.program("node/lib/node_modules/cline/dist/cli.js", 0o755);
    assert_eq!(
        method("cline", &cline, &machine.roots()),
        Method::Npm {
            package: "cline".into()
        }
    );
    assert_eq!(
        check_command("cline", &cline, &machine.roots())
            .unwrap()
            .unwrap()
            .args,
        ["view", "cline", "version"]
    );
}

#[test]
fn an_unscoped_npm_name_must_match_whole_and_a_wrong_package_is_unknown() {
    let machine = Machine::new();
    machine.program("node/bin/npm", 0o755);
    for (id, relative) in [
        ("droid", "node/lib/node_modules/droid-helper/bin/droid"),
        ("cline", "node/lib/node_modules/cline-evil/cli.js"),
        ("opencode", "node/lib/node_modules/opencode-ai/bin/opencode"),
        ("kimi", "node/lib/node_modules/kimi-cli/kimi"),
        (
            "pi",
            "node/lib/node_modules/@mariozechner/pi-coding-agent/dist/cli.js",
        ),
    ] {
        let real = machine.program(relative, 0o755);
        assert_eq!(method(id, &real, &machine.roots()), Method::Unknown, "{id}");
    }
}

#[test]
fn every_catalog_harness_but_rcli_has_one_recipe_with_an_https_guide_and_plain_names() {
    let mut seen = 0;
    for spec in crate::harnesses::specs() {
        if spec.id == "rcli" {
            assert!(guide(spec.id).is_none());
            continue;
        }
        let link = guide(spec.id).unwrap_or_else(|| panic!("{} has no recipe", spec.id));
        assert!(link.starts_with("https://"), "{}", spec.id);
        seen += 1;
    }
    assert_eq!(seen, 22);
    for name in table_names() {
        assert!(
            name.bytes().all(|byte| byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || b"@/._-=".contains(&byte)),
            "{name}"
        );
        assert!(!name.starts_with('-') || name.starts_with("--"), "{name}");
    }
}

#[test]
fn brew_outdated_json_names_a_newer_formula_too() {
    let formula = Method::HomebrewFormula {
        formula: "aider".into(),
    };
    let outdated = r#"{"formulae":[{"name":"aider","installed_versions":["0.86.1"],"current_version":"0.86.2","pinned":false,"pinned_version":null}],"casks":[]}"#;
    assert_eq!(
        latest(&formula, outdated, "0.86.1"),
        Latest::Newer {
            version: "0.86.2".into()
        }
    );
    assert_eq!(
        latest(&formula, r#"{"formulae":[],"casks":[]}"#, "0.86.2"),
        Latest::UpToDate
    );
}

#[test]
fn the_installed_version_is_found_among_the_first_words_of_other_harnesses_too() {
    assert_eq!(
        installed_version("aider 0.86.2\n").as_deref(),
        Some("0.86.2")
    );
    assert_eq!(installed_version("v1.5.2\n").as_deref(), Some("1.5.2"));
    assert_eq!(
        installed_version("Kiro CLI 3.2.0\n").as_deref(),
        Some("3.2.0")
    );
    assert_eq!(installed_version("2026.11.10-731fb78\n"), None);
}

fn unreadable() -> Latest {
    Latest::Failed {
        code: "harness_update_check_unreadable".into(),
    }
}

#[test]
fn each_self_updating_harness_has_one_fixed_https_pointer_and_no_other_harness_has_any() {
    let claude = pointer("claude-code").unwrap();
    let codex = pointer("codex").unwrap();
    assert_eq!(
        claude.url,
        "https://downloads.claude.ai/claude-code-releases/latest"
    );
    assert_eq!(
        codex.url,
        "https://releases.openai.com/codex/channels/latest"
    );
    assert_eq!(pointer("claude").unwrap().url, claude.url);
    for id in ["rcli", "aider", "opencode", "gemini-cli", "no-such-harness"] {
        assert!(pointer(id).is_none(), "{id}");
    }
    for pointer in [claude, codex] {
        assert!(pointer.url.starts_with("https://"));
        assert!(!pointer.url.contains(['?', '#', '@', ' ']));
        assert!(pointer.bytes_max <= POINTER_BYTES_MAX);
    }
}

#[test]
fn the_claude_pointer_is_one_bare_version_and_any_other_body_is_unreadable() {
    let claude = pointer("claude-code").unwrap();
    assert_eq!(
        latest_from_pointer(claude, b"2.1.322\n", "2.1.309"),
        Latest::Newer {
            version: "2.1.322".into()
        }
    );
    assert_eq!(
        latest_from_pointer(claude, b"2.1.322", "2.1.322"),
        Latest::UpToDate
    );
    assert_eq!(
        latest_from_pointer(claude, b"2.1.311", "2.1.322"),
        Latest::UpToDate
    );
    for body in [
        &b"<html>Sign in</html>"[..],
        b"2.1.322\nrm -rf /",
        b"2.1.322; curl x",
        b"",
        b"v2.1.322",
        b"2.1.322-beta.1",
        b"2",
        b"2.1.\xff",
    ] {
        assert_eq!(
            latest_from_pointer(claude, body, "2.1.309"),
            unreadable(),
            "{body:?}"
        );
    }
}

#[test]
fn the_codex_pointer_gives_the_tag_without_its_prefix_and_reads_nothing_else_from_the_reply() {
    let codex = pointer("codex").unwrap();
    let body = br#"{"tag_name":"rust-v0.167.1","assets":[{"name":"codex-aarch64-apple-darwin.tar.gz","url":"https://example.com/9.9.9"}]}"#;
    assert_eq!(
        latest_from_pointer(codex, body, "0.167.0"),
        Latest::Newer {
            version: "0.167.1".into()
        }
    );
    assert_eq!(
        latest_from_pointer(codex, body, "0.167.1"),
        Latest::UpToDate
    );
    for body in [
        &br#"{"tag_name":"v0.167.1"}"#[..],
        br#"{"tag_name":"rust-v0.167.1-alpha.3"}"#,
        br#"{"tag_name":7}"#,
        br#"{"assets":[]}"#,
        b"not json",
        br#"{"tag_name":"rust-v0.167.1\u0000"}"#,
        b"0.167.1",
    ] {
        assert_eq!(
            latest_from_pointer(codex, body, "0.167.0"),
            unreadable(),
            "{body:?}"
        );
    }
}

#[test]
fn every_recipe_names_at_most_one_homebrew_package_the_app_may_install() {
    for spec in crate::harnesses::specs() {
        assert!(install_names(spec.id).len() <= 1, "{}", spec.id);
    }
    assert_eq!(install_names("kimi"), ["kimi-code"]);
    assert!(install_names("gemini-cli").is_empty());
}

#[test]
fn install_uses_homebrew_first_with_the_table_name_and_its_kind() {
    let machine = Machine::new();
    let brew = machine.program("brew/bin/brew", 0o755);
    machine.npm("brew");
    for (id, args) in [
        ("codex", ["install", "--cask", "codex"]),
        ("claude-code", ["install", "--cask", "claude-code"]),
        ("aider", ["install", "--formula", "aider"]),
        ("goose", ["install", "--formula", "block-goose-cli"]),
        ("kimi", ["install", "--formula", "kimi-code"]),
        ("copilot", ["install", "--cask", "copilot-cli"]),
        ("antigravity", ["install", "--cask", "antigravity-cli"]),
        ("opencode", ["install", "--formula", "opencode"]),
    ] {
        let fixed = install_command(id, &machine.roots()).unwrap();
        assert_eq!(fixed.program, brew, "{id}");
        assert_eq!(fixed.args, args, "{id}");
        assert_eq!(
            fixed.search,
            vec![machine.root.path().join("brew/bin")],
            "{id}"
        );
    }
}

#[test]
fn install_uses_npm_when_homebrew_is_absent_with_the_vendors_flags() {
    let machine = Machine::new();
    let npm = machine.npm("brew");
    for (id, args) in [
        ("codex", vec!["install", "-g", "@openai/codex"]),
        (
            "claude-code",
            vec!["install", "-g", "@anthropic-ai/claude-code@latest"],
        ),
        ("opencode", vec!["install", "-g", "@opencode/cli"]),
        (
            "pi",
            vec![
                "install",
                "-g",
                "--ignore-scripts",
                "@earendil-works/pi-coding-agent",
            ],
        ),
        (
            "openclaw",
            vec![
                "install",
                "-g",
                "--allow-scripts=openclaw",
                "openclaw@latest",
            ],
        ),
        ("continue", vec!["install", "-g", "@continuedev/cli"]),
        ("kimi", vec!["install", "-g", "@moonshot-ai/kimi-code"]),
    ] {
        let fixed = install_command(id, &machine.roots()).unwrap();
        assert_eq!(fixed.program, npm, "{id}");
        assert_eq!(fixed.args, args, "{id}");
    }
}

#[test]
fn a_harness_whose_homebrew_formula_is_deprecated_installs_with_npm_even_when_homebrew_is_there() {
    let machine = Machine::new();
    machine.program("brew/bin/brew", 0o755);
    let npm = machine.npm("brew");
    for (id, package) in [("gemini-cli", "@google/gemini-cli"), ("cline", "cline")] {
        let fixed = install_command(id, &machine.roots()).unwrap();
        assert_eq!(fixed.program, npm, "{id}");
        assert_eq!(fixed.args, ["install", "-g", package], "{id}");
    }
}

#[test]
fn a_homebrew_only_harness_without_homebrew_and_a_harness_with_no_package_get_no_install_command() {
    let machine = Machine::new();
    machine.npm("brew");
    for id in ["aider", "goose", "cursor-agent", "kiro", "antigravity"] {
        assert_eq!(
            install_command(id, &machine.roots()).unwrap_err(),
            NO_INSTALLER,
            "{id}"
        );
    }
    machine.program("brew/bin/brew", 0o755);
    for id in ["fx", "openhands"] {
        assert_eq!(
            install_command(id, &machine.roots()).unwrap_err(),
            NO_INSTALLER,
            "{id}"
        );
        assert!(guide(id).is_some());
    }
    assert_eq!(
        install_command("rcli", &machine.roots()).unwrap_err(),
        WITH_APP
    );
    assert_eq!(
        install_command("no-such-harness", &machine.roots()).unwrap_err(),
        UNKNOWN_INSTALL
    );
}

#[test]
fn where_casks_are_not_supported_a_cask_harness_falls_to_npm_or_to_the_guide() {
    let machine = Machine::new();
    machine.program("brew/bin/brew", 0o755);
    let npm = machine.npm("brew");
    let roots = Roots {
        casks: false,
        ..machine.roots()
    };
    assert_eq!(install_command("codex", &roots).unwrap().program, npm);
    assert_eq!(
        install_command("cursor-agent", &roots).unwrap_err(),
        NO_INSTALLER
    );
    assert_eq!(
        install_command("aider", &roots).unwrap().args,
        ["install", "--formula", "aider"]
    );
}

#[test]
fn an_unsafe_homebrew_is_skipped_for_npm_and_an_unsafe_npm_gives_no_command() {
    let machine = Machine::new();
    machine.program("brew/bin/brew", 0o777);
    let npm = machine.npm("brew");
    assert_eq!(
        install_command("codex", &machine.roots()).unwrap().program,
        npm
    );
    std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        install_command("codex", &machine.roots()).unwrap_err(),
        NO_INSTALLER
    );
}

#[test]
fn no_install_command_ever_names_a_look_alike_package_a_script_or_sudo() {
    let machine = Machine::new();
    machine.program("brew/bin/brew", 0o755);
    machine.npm("brew");
    let mut built = 0;
    for with_brew in [true, false] {
        if !with_brew {
            std::fs::remove_file(machine.root.path().join("brew/bin/brew")).unwrap();
        }
        for spec in crate::harnesses::specs() {
            let Ok(fixed) = install_command(spec.id, &machine.roots()) else {
                continue;
            };
            built += 1;
            let tool = fixed
                .program
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            assert!(tool == "brew" || tool == "npm", "{}", spec.id);
            assert_eq!(fixed.args[0], "install", "{}", spec.id);
            let package = *fixed.args.last().unwrap();
            for wrong in [
                "fx",
                "amp",
                "goose",
                "copilot",
                "cursor-agent",
                "antigravity",
                "kiro-cli",
                "goose-cli",
                "kimi-cli",
                "openhands",
                "opencode-ai",
                "@mariozechner/pi-coding-agent",
            ] {
                assert!(
                    !(package == wrong && (tool == "npm" || fixed.args[1] == "--formula")),
                    "{} would install {wrong}",
                    spec.id
                );
            }
            for arg in &fixed.args {
                assert!(
                    !arg.contains("sudo")
                        && !arg.contains("curl")
                        && !arg.contains("http")
                        && !arg.contains('|')
                        && !arg.contains(' ')
                        && !arg.contains('/')
                        || arg.starts_with('@'),
                    "{} {arg}",
                    spec.id
                );
            }
        }
    }
    assert_eq!(built, 20 + 15);
}

fn executable(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    crate::spawn_gate::guarded(|| std::fs::write(path, text)).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn script(machine: &Machine, relative: &str, body: &str, args: &[&'static str]) -> Fixed {
    let path = machine.root.path().join(relative);
    executable(&path, &format!("#!/bin/sh\n{body}"));
    std::fs::create_dir_all(machine.roots().home).unwrap();
    Fixed {
        program: path.clone(),
        args: args.to_vec(),
        search: vec![path.parent().unwrap().to_path_buf()],
        prefix: None,
    }
}

#[test]
fn a_run_passes_exactly_the_fixed_arguments_and_a_cleared_environment() {
    let machine = Machine::new();
    let out = machine.root.path().join("seen");
    let fixed = script(
        &machine,
        "bin/tool",
        &format!(
            "printf '%s\\n' \"$@\" > {0}.args\nenv > {0}.env\n",
            out.display()
        ),
        &["upgrade", "--cask", "codex"],
    );
    let ran = run(&fixed, &machine.roots().home, CHECK_TIME).unwrap();
    assert!(ran.success);
    assert_eq!(
        std::fs::read_to_string(format!("{}.args", out.display())).unwrap(),
        "upgrade\n--cask\ncodex\n"
    );
    let names: Vec<String> = std::fs::read_to_string(format!("{}.env", out.display()))
        .unwrap()
        .lines()
        .filter_map(|line| line.split('=').next().map(str::to_owned))
        .collect();
    for name in &names {
        assert!(
            [
                "HOME", "PATH", "USER", "LOGNAME", "LANG", "TMPDIR", "PWD", "SHLVL", "_", "OLDPWD"
            ]
            .contains(&name.as_str()),
            "{name}"
        );
    }
    assert!(names.iter().any(|name| name == "PATH"));
}

#[test]
fn the_search_path_is_the_tools_folder_then_the_system_folders_and_nothing_else() {
    let machine = Machine::new();
    let out = machine.root.path().join("path");
    let fixed = script(
        &machine,
        "bin/tool",
        &format!("printf '%s' \"$PATH\" > {}\n", out.display()),
        &[],
    );
    run(&fixed, &machine.roots().home, CHECK_TIME).unwrap();
    let path = std::fs::read_to_string(&out).unwrap();
    assert_eq!(
        path,
        format!(
            "{}:/usr/bin:/bin:/usr/sbin:/sbin",
            machine.root.path().join("bin").display()
        )
    );
}

#[test]
fn output_past_the_bound_is_dropped_and_the_tool_still_ends() {
    let machine = Machine::new();
    let fixed = script(
        &machine,
        "bin/tool",
        "i=0\nwhile [ $i -lt 40000 ]; do echo 0123456789012345678901234567890123456789; i=$((i+1)); done\n",
        &[],
    );
    let ran = run(&fixed, &machine.roots().home, CHECK_TIME).unwrap();
    assert!(ran.success);
    assert!(ran.output.len() <= SHOWN_BYTES_MAX);
}

#[test]
fn a_tool_past_its_time_is_ended_with_every_child_it_started() {
    let machine = Machine::new();
    let pid_file = machine.root.path().join("child.pid");
    let fixed = script(
        &machine,
        "bin/tool",
        &format!(
            "[ \"$1\" = warm ] && exit 0\nsleep 600 &\necho $! > {}\nsleep 600\n",
            pid_file.display()
        ),
        &[],
    );
    let warm = Fixed {
        args: vec!["warm"],
        ..fixed.clone()
    };
    assert!(
        run(&warm, &machine.roots().home, CHECK_TIME)
            .unwrap()
            .success
    );
    assert_eq!(
        run(&fixed, &machine.roots().home, Duration::from_secs(2)).unwrap_err(),
        "harness_update_timeout"
    );
    let child: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let gone = (0..40).any(|_| {
        std::thread::sleep(Duration::from_millis(50));
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(child).unwrap()).is_err()
    });
    assert!(gone);
}

#[test]
fn a_failing_tool_reports_failure_with_its_last_lines() {
    let machine = Machine::new();
    let fixed = script(
        &machine,
        "bin/tool",
        "echo Error: cask is pinned >&2\nexit 3\n",
        &[],
    );
    let ran = run(&fixed, &machine.roots().home, CHECK_TIME).unwrap();
    assert!(!ran.success);
    assert!(ran.output.contains("Error: cask is pinned"));
}

#[test]
fn a_tool_that_reads_input_gets_none() {
    let machine = Machine::new();
    let fixed = script(&machine, "bin/tool", "read answer || echo no-input\n", &[]);
    assert!(
        run(&fixed, &machine.roots().home, CHECK_TIME)
            .unwrap()
            .output
            .contains("no-input")
    );
}

#[test]
fn shown_output_hides_credential_shapes_and_the_home_folder() {
    let home = Path::new("/Users/owner");
    let text = masked(b"Authorization: Bearer sk-ant-api03-abcdefghijklmnopqrstuvwxyz012345\ninstalled to /Users/owner/.local/bin/claude\n", home);
    assert!(!text.contains("sk-ant-api03"));
    assert!(!text.contains("/Users/owner"));
    assert!(text.contains("~/.local/bin/claude"));
}

#[test]
fn shown_output_drops_terminal_control_sequences() {
    let text = masked(b"\x1b[32mUpdated\x1b[0m\r\n", Path::new("/Users/owner"));
    assert_eq!(text.trim(), "Updated");
}

#[test]
fn the_root_user_is_refused() {
    assert_eq!(refused_user(0), Some("harness_update_as_root"));
    assert_eq!(refused_user(501), None);
}

fn serve_once(reply: Vec<u8>) -> (String, std::thread::JoinHandle<String>) {
    use std::io::{Read, Write};
    let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}/pointer",
        listener.local_addr().unwrap().port()
    );
    let served = std::thread::spawn(move || {
        let (mut stream, _) = crate::spawn_gate::accept_tcp_std(&listener).unwrap();
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n")
            && head.len() < 8192
            && stream.read(&mut byte).unwrap_or(0) == 1
        {
            head.push(byte[0]);
        }
        let _ = stream.write_all(&reply);
        String::from_utf8_lossy(&head).to_ascii_lowercase()
    });
    (url, served)
}

fn answer(status: &str, body: &[u8]) -> Vec<u8> {
    let mut reply = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    reply.extend_from_slice(body);
    reply
}

#[test]
fn a_pointer_read_returns_the_body_within_its_bound() {
    let (url, served) = serve_once(answer("200 OK", b"2.1.322\n"));
    assert_eq!(read_pointer(&url, 64, POINTER_TIME).unwrap(), b"2.1.322\n");
    served.join().unwrap();
}

#[test]
fn a_pointer_request_carries_no_cookie_no_credentials_and_a_plain_user_agent() {
    let (url, served) = serve_once(answer("200 OK", b"2.1.322\n"));
    read_pointer(&url, 64, POINTER_TIME).unwrap();
    let head = served.join().unwrap();
    assert!(head.starts_with("get /pointer http/1.1\r\n"));
    assert!(head.contains("\r\nuser-agent: crazyagent/"));
    for header in [
        "\r\ncookie:",
        "\r\nauthorization:",
        "\r\nreferer:",
        "\r\nx-",
    ] {
        assert!(!head.contains(header), "{header}");
    }
}

#[test]
fn a_pointer_reply_over_its_bound_is_a_failed_lookup_and_none_of_it_is_returned() {
    let (url, _served) = serve_once(answer("200 OK", &[b'9'; 100]));
    assert_eq!(
        read_pointer(&url, 64, POINTER_TIME).unwrap_err(),
        LOOKUP_FAILED
    );
}

#[test]
fn a_pointer_server_that_answers_an_error_is_a_failed_lookup() {
    let (url, _served) = serve_once(answer("404 Not Found", b"<html>missing</html>"));
    assert_eq!(
        read_pointer(&url, 64, POINTER_TIME).unwrap_err(),
        LOOKUP_FAILED
    );
}

#[test]
fn a_pointer_server_that_never_answers_is_a_failed_lookup_within_the_time_limit() {
    let listener = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}/pointer",
        listener.local_addr().unwrap().port()
    );
    let started = std::time::Instant::now();
    assert_eq!(
        read_pointer(&url, 64, Duration::from_secs(1)).unwrap_err(),
        LOOKUP_FAILED
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(listener);
}

#[test]
fn a_pointer_url_that_is_not_https_or_loopback_is_refused_before_any_request() {
    assert_eq!(
        read_pointer(
            "http://downloads.claude.ai/claude-code-releases/latest",
            64,
            POINTER_TIME
        )
        .unwrap_err(),
        LOOKUP_FAILED
    );
    assert_eq!(
        read_pointer("file:///etc/hosts", 64, POINTER_TIME).unwrap_err(),
        LOOKUP_FAILED
    );
}

const NPM_LIKE: &str = "#!/bin/sh\ntarget=\"${npm_config_prefix:-$(/usr/bin/sed -n 's/^prefix=//p' \"$HOME/.npmrc\")}\"\n/bin/mkdir -p \"$target\" && echo \"$*\" > \"$target/installed\"\n";

#[test]
fn a_cask_check_also_asks_about_casks_that_update_themselves() {
    let machine = Machine::new();
    machine.program("brew/bin/brew", 0o755);
    let droid = machine.program("brew/Caskroom/droid/1.7.1/droid", 0o755);
    let check = check_command("droid", &droid, &machine.roots())
        .unwrap()
        .unwrap();
    assert_eq!(
        check.args,
        [
            "outdated",
            "--cask",
            "--greedy-auto-updates",
            "--json=v2",
            "droid"
        ]
    );
    assert!(check.args.len() <= ARGS_MAX);
    assert!(shown(&check).ends_with("--greedy-auto-updates --json=v2 droid"));
}

#[test]
fn a_self_update_is_refused_when_the_entry_leads_to_another_install() {
    let machine = Machine::new();
    let real = machine.program("home/.local/share/claude/versions/2.1.322", 0o755);
    let other = machine.program("home/.local/share/claude/versions/2.1.300", 0o755);
    let entry = machine.root.path().join("home/.local/bin/claude");
    std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&other, &entry).unwrap();
    assert_eq!(
        update_command("claude-code", &entry, &real, &machine.roots()).unwrap_err(),
        UNKNOWN_INSTALL
    );
    assert!(update_command("claude-code", &entry, &other, &machine.roots()).is_ok());
}

#[test]
fn npm_installs_into_the_prefix_the_harness_was_found_in_whatever_the_owners_npmrc_says() {
    let machine = Machine::new();
    let home = machine.roots().home;
    let elsewhere = machine.root.path().join("elsewhere");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join(".npmrc"),
        format!("prefix={}\n", elsewhere.display()),
    )
    .unwrap();
    executable(&machine.npm("node"), NPM_LIKE);
    let real = machine.program("node/lib/node_modules/@openai/codex/bin/codex.js", 0o755);
    let update = update_command("codex", &real, &real, &machine.roots()).unwrap();
    assert!(run(&update, &home, CHECK_TIME).unwrap().success);
    assert!(machine.root.path().join("node/installed").exists());
    executable(&machine.npm("brew"), NPM_LIKE);
    let install = install_command("codex", &machine.roots()).unwrap();
    assert_eq!(install.program, machine.root.path().join("brew/bin/npm"));
    assert!(run(&install, &home, CHECK_TIME).unwrap().success);
    assert!(machine.root.path().join("brew/installed").exists());
    assert!(!elsewhere.join("installed").exists());
}

/// The groups this user is in, as `/usr/bin/id -G` lists them.
fn user_groups() -> Vec<u32> {
    let listed = crate::spawn_gate::output(
        std::process::Command::new("/usr/bin/id")
            .arg("-G")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(listed.status.success());
    String::from_utf8_lossy(&listed.stdout)
        .split_whitespace()
        .filter_map(|group| group.parse::<u32>().ok())
        .collect()
}

#[test]
fn a_tool_folder_that_another_group_can_write_is_refused() {
    let machine = Machine::new();
    let brew = machine.program("brew/bin/brew", 0o755);
    let real = machine.program("brew/Caskroom/codex/0.167.1/bin/codex", 0o755);
    let folder = brew.parent().unwrap();
    assert!(update_command("codex", &real, &real, &machine.roots()).is_ok());
    let own = rustix::process::getegid().as_raw();
    let other = user_groups()
        .into_iter()
        .find(|group| *group != own && *group != ADMIN_GROUP);
    let Some(other) = other else {
        return;
    };
    std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o775)).unwrap();
    std::os::unix::fs::chown(folder, None, Some(other)).unwrap();
    assert_eq!(
        update_command("codex", &real, &real, &machine.roots()).unwrap_err(),
        TOOL_UNSAFE
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_tool_folder_the_users_own_group_can_write_is_refused_on_macos() {
    let machine = Machine::new();
    let brew = machine.program("brew/bin/brew", 0o755);
    let real = machine.program("brew/Caskroom/codex/0.167.1/bin/codex", 0o755);
    let folder = brew.parent().unwrap();
    let own = rustix::process::getegid().as_raw();
    if own == ADMIN_GROUP {
        return;
    }
    std::os::unix::fs::chown(folder, None, Some(own)).unwrap();
    assert!(update_command("codex", &real, &real, &machine.roots()).is_ok());
    std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o775)).unwrap();
    assert_eq!(
        update_command("codex", &real, &real, &machine.roots()).unwrap_err(),
        TOOL_UNSAFE
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_tool_folder_the_macos_administrators_can_write_is_accepted() {
    let machine = Machine::new();
    let brew = machine.program("brew/bin/brew", 0o755);
    let real = machine.program("brew/Caskroom/codex/0.167.1/bin/codex", 0o755);
    let folder = brew.parent().unwrap();
    if !user_groups().contains(&ADMIN_GROUP) {
        return;
    }
    std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o775)).unwrap();
    std::os::unix::fs::chown(folder, None, Some(ADMIN_GROUP)).unwrap();
    assert!(update_command("codex", &real, &real, &machine.roots()).is_ok());
}

#[cfg(not(target_os = "macos"))]
#[test]
fn a_tool_folder_the_users_own_group_can_write_is_accepted_off_macos() {
    let machine = Machine::new();
    let brew = machine.program("brew/bin/brew", 0o755);
    let real = machine.program("brew/Caskroom/codex/0.167.1/bin/codex", 0o755);
    let folder = brew.parent().unwrap();
    let own = rustix::process::getegid().as_raw();
    std::os::unix::fs::chown(folder, None, Some(own)).unwrap();
    std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o775)).unwrap();
    assert!(update_command("codex", &real, &real, &machine.roots()).is_ok());
}

use crate::harnesses::{HarnessEntry, Search};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

const WAIT_STEPS: usize = 400;
const WAIT_STEP: Duration = Duration::from_millis(25);

fn db() -> Connection {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    db
}

fn entry(id: &str, executable: Option<PathBuf>) -> HarnessEntry {
    let mut entry = crate::harnesses::catalog(&Search::new(Vec::new(), None, Vec::new()))
        .into_iter()
        .find(|entry| entry.id == id)
        .unwrap();
    assert!(entry.executable.is_none());
    entry.executable = executable;
    entry
}

fn found_in(folder: &Path, id: &str) -> Option<PathBuf> {
    crate::harnesses::catalog(&Search::new(vec![folder.to_path_buf()], None, Vec::new()))
        .into_iter()
        .find(|entry| entry.id == id)
        .and_then(|entry| entry.executable)
}

struct Case {
    machine: Machine,
    db: Connection,
    updates: Arc<Updates>,
    entry: HarnessEntry,
    asked: Arc<Mutex<Vec<String>>>,
    gate: Option<PathBuf>,
}

impl Case {
    fn new(machine: Machine, entry: HarnessEntry, gate: Option<PathBuf>) -> Self {
        let case = Self {
            machine,
            db: db(),
            updates: Updates::new(),
            entry,
            asked: Arc::default(),
            gate,
        };
        case.reading(Err(LOOKUP_FAILED))
    }
    fn reading(mut self, reply: Result<&'static [u8], &'static str>) -> Self {
        let asked = self.asked.clone();
        self.updates = Updates::with_reader(Box::new(move |pointer| {
            asked.lock().unwrap().push(pointer.url.to_owned());
            reply.map(<[u8]>::to_vec)
        }));
        self
    }
    fn pointer_body(self, body: &'static [u8]) -> Self {
        self.reading(Ok(body))
    }
    fn pointer_failure(self) -> Self {
        self.reading(Err(LOOKUP_FAILED))
    }
    fn pointer_reads(&self) -> usize {
        self.asked.lock().unwrap().len()
    }
    fn pointer_urls(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
    fn versions(&self, look: bool) -> HarnessVersions {
        self.updates
            .versions(&self.db, &self.entry, &self.machine.roots(), look)
    }
    fn start(&self) -> Result<(), &'static str> {
        self.updates
            .start(&self.db, &self.entry, &self.machine.roots(), |_| {})
    }
    fn start_telling(&self) -> std::sync::mpsc::Receiver<bool> {
        let (ended, told) = std::sync::mpsc::channel();
        self.updates
            .start(&self.db, &self.entry, &self.machine.roots(), move |ok| {
                ended.send(ok).unwrap();
            })
            .unwrap();
        told
    }
    fn bin(&self) -> PathBuf {
        self.machine.root.path().join("brew/bin")
    }
    fn install(&self) -> Result<(), &'static str> {
        let (bin, id) = (self.bin(), self.entry.id.clone());
        self.updates
            .install(&self.db, &self.entry, &self.machine.roots(), move || {
                found_in(&bin, &id)
            })
    }
    fn found_after(&self) -> Option<PathBuf> {
        found_in(&self.bin(), &self.entry.id)
    }
    fn tool_arguments(&self) -> Vec<String> {
        let seen = std::fs::read_to_string(self.machine.root.path().join("tool.args")).unwrap();
        seen.lines().map(str::to_owned).collect()
    }
    fn finish(&self) {
        if let Some(gate) = &self.gate {
            std::fs::write(gate, "go\n").unwrap();
        }
        let idle = (0..WAIT_STEPS).any(|_| {
            std::thread::sleep(WAIT_STEP);
            self.updates.idle()
        });
        assert!(idle);
    }
}

fn gate(machine: &Machine) -> PathBuf {
    machine.root.path().join("gate")
}

fn wait_for(gate: &Option<PathBuf>) -> String {
    gate.as_ref().map_or(String::new(), |gate| {
        format!(
            "i=0\nwhile [ ! -e {} ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done\n",
            gate.display()
        )
    })
}

fn native_case(id: &str, update: &str, gated: bool) -> Case {
    let machine = Machine::new();
    let root = machine.root.path().to_path_buf();
    let versions = root.join("home/.local/share/claude/versions");
    let launcher = root.join("home/.local/bin/claude");
    let gate = gated.then(|| gate(&machine));
    let wait = wait_for(&gate);
    let update = update.replace(
        "{swap}",
        &format!(
            "ln -sfn {} {}",
            versions.join("B").display(),
            launcher.display()
        ),
    );
    executable(
        &versions.join("A"),
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n--version) echo '1.0.0 (Claude Code)';;\nupdate) {wait}{update};;\nesac\n"
        ),
    );
    executable(
        &versions.join("B"),
        "#!/bin/sh\necho '2.0.0 (Claude Code)'\n",
    );
    std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(versions.join("A"), &launcher).unwrap();
    Case::new(machine, entry(id, Some(launcher)), gate)
}

fn update_case(id: &str) -> Case {
    native_case(id, "{swap}", false)
}

fn slow_update_case(id: &str) -> Case {
    native_case(id, "{swap}", true)
}

fn failing_update_case(id: &str) -> Case {
    native_case(id, "echo 'Error: could not update' >&2; exit 3", false)
}

fn update_case_at(id: &str, relative: &str) -> Case {
    let machine = Machine::new();
    let path = machine.root.path().join(relative);
    let ran = machine.root.path().join("ran");
    executable(
        &path,
        &format!(
            "#!/bin/sh\necho ran > {}\necho '1.0.0 (Claude Code)'\n",
            ran.display()
        ),
    );
    std::fs::create_dir_all(machine.roots().home).unwrap();
    Case::new(machine, entry(id, Some(path)), None)
}

fn update_case_missing(id: &str) -> Case {
    Case::new(Machine::new(), entry(id, None), None)
}

fn brew_case(id: &str, install: Option<&str>, gated: bool) -> Case {
    let machine = Machine::new();
    let root = machine.root.path().to_path_buf();
    std::fs::create_dir_all(machine.roots().home).unwrap();
    let gate = gated.then(|| gate(&machine));
    if let Some(install) = install {
        let wait = wait_for(&gate);
        let binary = root.join("brew/bin").join(id);
        let write = format!(
            "printf '#!/bin/sh\\necho \"{id} 3.0.0\"\\n' > {0}\nchmod 755 {0}",
            binary.display()
        );
        let install = install.replace("{write}", &write);
        executable(
            &root.join("brew/bin/brew"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n{wait}{install}\n",
                root.join("tool.args").display()
            ),
        );
    }
    Case::new(machine, entry(id, None), gate)
}

fn install_case(id: &str) -> Case {
    brew_case(id, Some("{write}"), false)
}

fn slow_install_case(id: &str) -> Case {
    brew_case(id, Some("{write}"), true)
}

fn install_case_without_tools(id: &str) -> Case {
    brew_case(id, None, false)
}

fn install_case_that_writes_nothing(id: &str) -> Case {
    brew_case(id, Some("true"), false)
}

fn failing_install_case(id: &str) -> Case {
    brew_case(id, Some("echo 'Error: no formula' >&2; exit 1"), false)
}

#[test]
fn the_settings_default_to_check_and_update_on_the_button_with_online_lookup_and_keep_each_option()
{
    let db = db();
    assert_eq!(
        settings(&db).unwrap(),
        UpdateSettings {
            mode: UpdateMode::CheckAndUpdate,
            when: CheckWhen::OnButton,
            online: OnlineLookup::On,
            install: InstallFromApp::On
        }
    );
    set_settings(
        &db,
        UpdateSettings {
            install: InstallFromApp::Off,
            ..UpdateSettings::default()
        },
    )
    .unwrap();
    assert_eq!(settings(&db).unwrap().install, InstallFromApp::Off);
    for mode in [
        UpdateMode::CheckOnly,
        UpdateMode::Off,
        UpdateMode::CheckAndUpdate,
    ] {
        for when in [CheckWhen::OnOpen, CheckWhen::OnButton] {
            for online in [OnlineLookup::Off, OnlineLookup::On] {
                let wanted = UpdateSettings {
                    mode,
                    when,
                    online,
                    install: InstallFromApp::On,
                };
                set_settings(&db, wanted).unwrap();
                assert_eq!(settings(&db).unwrap(), wanted);
            }
        }
    }
}

fn with_mode(mode: UpdateMode) -> UpdateSettings {
    UpdateSettings {
        mode,
        ..UpdateSettings::default()
    }
}

#[test]
fn a_self_updating_install_reads_its_vendor_pointer_only_on_a_look_and_only_with_online_lookup_on()
{
    let case = update_case("claude-code").pointer_body(b"2.0.0\n");
    assert_eq!(case.versions(false).latest, Latest::NotChecked);
    assert_eq!(case.pointer_reads(), 0);
    assert_eq!(
        case.versions(true).latest,
        Latest::Newer {
            version: "2.0.0".into()
        }
    );
    assert_eq!(case.pointer_reads(), 1);
    assert_eq!(
        case.pointer_urls(),
        vec!["https://downloads.claude.ai/claude-code-releases/latest"]
    );
    assert_eq!(
        case.versions(false).latest,
        Latest::Newer {
            version: "2.0.0".into()
        }
    );
    assert_eq!(case.pointer_reads(), 1);
    set_settings(
        &case.db,
        UpdateSettings {
            online: OnlineLookup::Off,
            ..UpdateSettings::default()
        },
    )
    .unwrap();
    assert_eq!(case.versions(true).latest, Latest::NoCheck);
    assert_eq!(case.pointer_reads(), 1);
}

#[test]
fn a_failed_or_unreadable_pointer_says_could_not_check_and_update_stays_offered() {
    let failed = update_case("claude-code").pointer_failure().versions(true);
    assert_eq!(
        failed.latest,
        Latest::Failed {
            code: "harness_update_lookup_failed".into()
        }
    );
    assert!(failed.command.is_some());
    let unreadable = update_case("claude-code")
        .pointer_body(b"<html>Sign in</html>")
        .versions(true);
    assert_eq!(
        unreadable.latest,
        Latest::Failed {
            code: "harness_update_check_unreadable".into()
        }
    );
}

#[test]
fn a_package_manager_install_an_unknown_install_and_updates_off_never_read_a_vendor_pointer() {
    let brew = update_case_at("claude-code", "brew/Caskroom/claude-code/1.0.0/claude")
        .pointer_body(b"2.0.0\n");
    brew.versions(true);
    assert_eq!(brew.pointer_reads(), 0);
    let unknown = update_case_at("claude-code", "usr/bin/claude").pointer_body(b"2.0.0\n");
    unknown.versions(true);
    assert_eq!(unknown.pointer_reads(), 0);
    let off = update_case("claude-code").pointer_body(b"2.0.0\n");
    set_settings(&off.db, with_mode(UpdateMode::Off)).unwrap();
    off.versions(true);
    assert_eq!(off.pointer_reads(), 0);
}

#[test]
fn off_refuses_a_look_and_an_update_and_check_only_refuses_only_the_update() {
    let case = update_case("claude-code");
    set_settings(&case.db, with_mode(UpdateMode::Off)).unwrap();
    assert_eq!(case.versions(true).latest, Latest::NotChecked);
    assert_eq!(case.start().unwrap_err(), "harness_update_off");
    set_settings(&case.db, with_mode(UpdateMode::CheckOnly)).unwrap();
    assert_eq!(case.start().unwrap_err(), "harness_update_check_only");
    assert!(case.versions(true).installed.is_some());
}

#[test]
fn versions_name_the_method_the_exact_update_line_and_why_update_is_not_offered() {
    let known = update_case("claude-code").versions(false);
    assert_eq!(known.method, Method::SelfUpdate);
    assert!(
        known
            .command
            .as_deref()
            .unwrap()
            .ends_with("/claude update")
    );
    assert_eq!(known.refusal, None);
    let unknown = update_case_at("claude-code", "usr/bin/claude").versions(false);
    assert_eq!(unknown.command, None);
    assert_eq!(unknown.refusal.as_deref(), Some(UNKNOWN_INSTALL));
    assert_eq!(
        unknown.guide.as_deref(),
        Some("https://code.claude.com/docs/en/setup")
    );
}

#[test]
fn a_second_update_while_one_runs_is_refused_and_the_first_reports_running_then_its_outcome() {
    let case = slow_update_case("claude-code");
    case.start().unwrap();
    assert!(case.versions(false).running);
    assert_eq!(case.start().unwrap_err(), "harness_update_busy");
    case.finish();
    let done = case.versions(false);
    assert!(!done.running);
    let last = done.last.unwrap();
    assert!(last.ok);
    assert_eq!(
        (last.from.as_deref(), last.to.as_deref()),
        (Some("1.0.0"), Some("2.0.0"))
    );
}

#[test]
fn a_failed_update_keeps_its_masked_last_lines_and_the_old_version() {
    let case = failing_update_case("claude-code");
    case.start().unwrap();
    case.finish();
    let last = case.versions(false).last.unwrap();
    assert!(!last.ok);
    assert_eq!(last.code.as_deref(), Some("harness_update_failed"));
    assert!(last.output.contains("Error:"));
    assert_eq!(last.to.as_deref(), Some("1.0.0"));
}

#[test]
fn the_follow_up_of_an_update_is_told_whether_the_update_ended_well() {
    let wait = Duration::from_secs(30);
    let failing = failing_update_case("claude-code");
    let told = failing.start_telling();
    assert_eq!(told.recv_timeout(wait), Ok(false));
    let working = update_case("claude-code");
    let told = working.start_telling();
    assert_eq!(told.recv_timeout(wait), Ok(true));
}

#[test]
fn a_harness_that_is_not_installed_or_not_in_the_catalog_is_refused() {
    assert_eq!(
        update_case_missing("codex").start().unwrap_err(),
        "harness_update_not_installed"
    );
}

#[test]
fn versions_of_a_missing_harness_name_the_exact_install_line_and_always_the_guide() {
    let case = install_case("aider");
    let versions = case.versions(false);
    assert!(
        versions
            .install
            .as_deref()
            .unwrap()
            .ends_with("/brew install --formula aider")
    );
    assert_eq!(versions.command, None);
    assert_eq!(
        versions.guide.as_deref(),
        Some("https://aider.chat/docs/install.html")
    );
    let none = install_case_without_tools("aider").versions(false);
    assert_eq!(none.install, None);
    assert_eq!(none.refusal.as_deref(), Some(NO_INSTALLER));
    assert_eq!(
        none.guide.as_deref(),
        Some("https://aider.chat/docs/install.html")
    );
}

#[test]
fn an_install_runs_the_fixed_line_and_the_harness_is_found_right_after() {
    let case = install_case("aider");
    case.install().unwrap();
    case.finish();
    assert_eq!(case.tool_arguments(), vec!["install", "--formula", "aider"]);
    let last = case.versions(false).last.unwrap();
    assert!(last.ok);
    assert_eq!((last.from, last.to.as_deref()), (None, Some("3.0.0")));
    assert!(case.found_after().is_some());
}

#[test]
fn an_install_is_refused_when_installed_when_the_setting_is_off_and_while_another_job_runs() {
    assert_eq!(
        update_case("claude-code").install().unwrap_err(),
        "harness_install_already"
    );
    let off = install_case("aider");
    set_settings(
        &off.db,
        UpdateSettings {
            install: InstallFromApp::Off,
            ..UpdateSettings::default()
        },
    )
    .unwrap();
    assert_eq!(off.install().unwrap_err(), "harness_install_off");
    assert_eq!(off.versions(false).install, None);
    assert!(off.versions(false).guide.is_some());
    let busy = slow_install_case("aider");
    busy.install().unwrap();
    assert_eq!(busy.install().unwrap_err(), "harness_update_busy");
    busy.finish();
}

#[test]
fn updates_off_does_not_turn_install_off_and_install_off_does_not_turn_updates_off() {
    let case = install_case("aider");
    set_settings(&case.db, with_mode(UpdateMode::Off)).unwrap();
    assert!(case.versions(false).install.is_some());
    let installed = update_case("claude-code");
    set_settings(
        &installed.db,
        UpdateSettings {
            install: InstallFromApp::Off,
            ..UpdateSettings::default()
        },
    )
    .unwrap();
    assert!(installed.versions(false).command.is_some());
}

#[test]
fn an_install_that_ends_well_but_leaves_no_binary_says_so_and_a_failed_one_keeps_its_last_lines() {
    let nothing = install_case_that_writes_nothing("aider");
    nothing.install().unwrap();
    nothing.finish();
    let last = nothing.versions(false).last.unwrap();
    assert!(!last.ok);
    assert_eq!(last.code.as_deref(), Some("harness_install_not_found"));
    let failing = failing_install_case("aider");
    failing.install().unwrap();
    failing.finish();
    let last = failing.versions(false).last.unwrap();
    assert_eq!(last.code.as_deref(), Some("harness_install_failed"));
    assert!(last.output.contains("Error:"));
}

#[test]
fn an_unrecognized_install_of_a_harness_crazyagent_cannot_start_is_never_run() {
    let look_alike = update_case_at("fx", "brew/Cellar/fx/40.0.1/bin/fx");
    let versions = look_alike.versions(true);
    assert_eq!(versions.method, Method::Unknown);
    assert_eq!(versions.installed, None);
    assert_eq!(versions.latest, Latest::NotChecked);
    assert!(!look_alike.machine.root.path().join("ran").exists());
    let system = update_case_at("claude-code", "usr/bin/claude");
    assert_eq!(system.versions(false).installed.as_deref(), Some("1.0.0"));
    assert!(system.machine.root.path().join("ran").exists());
}

#[test]
fn an_npm_harness_on_a_mac_with_no_npm_needs_node_and_a_harness_with_no_package_has_no_installer() {
    let machine = Machine::new();
    for id in ["codex", "claude-code", "gemini-cli"] {
        assert_eq!(
            install_command(id, &machine.roots()).unwrap_err(),
            "harness_install_needs_node",
            "{id}"
        );
    }
    for id in ["aider", "goose", "cursor-agent"] {
        assert_eq!(
            install_command(id, &machine.roots()).unwrap_err(),
            NO_INSTALLER,
            "{id}"
        );
    }
    let missing = Case::new(Machine::new(), entry("codex", None), None);
    assert_eq!(
        missing.versions(false).refusal.as_deref(),
        Some("harness_install_needs_node")
    );
}

fn listed(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

#[test]
fn a_list_is_queued_only_when_each_id_is_a_catalog_harness_once_and_never_rcli() {
    let updates = Updates::new();
    let db = db();
    for refused in [
        listed(&[]),
        listed(&["no-such-harness"]),
        listed(&["rcli"]),
        listed(&["codex", "codex"]),
        listed(&["claude", "claude-code"]),
        vec!["codex".to_owned(); crate::harnesses::specs().len() + 1],
    ] {
        assert_eq!(
            updates.queue(&db, &refused).unwrap_err(),
            "harness_install_list_invalid",
            "{refused:?}"
        );
        assert!(updates.queued().is_empty());
    }
    assert_eq!(
        updates.queue(&db, &listed(&["claude", "codex"])).unwrap(),
        vec!["claude-code", "codex"]
    );
    assert_eq!(updates.queued(), listed(&["claude-code", "codex"]));
    assert_eq!(
        updates.queue(&db, &listed(&["aider"])).unwrap_err(),
        "harness_update_busy"
    );
    let off = self::db();
    set_settings(
        &off,
        UpdateSettings {
            install: InstallFromApp::Off,
            ..UpdateSettings::default()
        },
    )
    .unwrap();
    assert_eq!(
        Updates::new().queue(&off, &listed(&["codex"])).unwrap_err(),
        "harness_install_off"
    );
}

#[test]
fn a_queued_list_installs_in_order_one_at_a_time_and_a_failure_does_not_stop_the_rest() {
    let case = slow_install_case("aider");
    let ids = listed(&["aider", "goose", "codex"]);
    assert_eq!(
        case.updates.queue(&case.db, &ids).unwrap(),
        vec!["aider", "goose", "codex"]
    );
    let seen: Arc<Mutex<Vec<(&'static str, bool)>>> = Arc::default();
    let drain = {
        let (updates, seen) = (case.updates.clone(), seen.clone());
        let (roots, bin) = (case.machine.roots(), case.bin());
        std::thread::spawn(move || {
            let db = db();
            let worker = updates.clone();
            updates.drain(
                move |id| {
                    seen.lock().unwrap().push((id, worker.idle()));
                    assert_eq!(worker.queued().first().map(String::as_str), Some(id));
                    match id {
                        "aider" => {
                            let bin = bin.clone();
                            worker.install(&db, &entry(id, None), &roots, move || {
                                found_in(&bin, "aider")
                            })
                        }
                        "goose" => Err(NO_INSTALLER),
                        _ => Ok(()),
                    }
                },
                WAIT_STEP,
                WAIT_STEP * u32::try_from(WAIT_STEPS).unwrap(),
            );
        })
    };
    let running = (0..WAIT_STEPS).any(|_| {
        std::thread::sleep(WAIT_STEP);
        !case.updates.idle()
    });
    assert!(running);
    assert_eq!(case.updates.queued(), listed(&["goose", "codex"]));
    case.finish();
    drain.join().unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![("aider", true), ("goose", true), ("codex", true)]
    );
    assert!(case.updates.queued().is_empty());
    assert!(case.versions(false).last.unwrap().ok);
    let goose = case.updates.versions(
        &case.db,
        &entry("goose", None),
        &case.machine.roots(),
        false,
    );
    assert_eq!(goose.last.unwrap().code.as_deref(), Some(NO_INSTALLER));
}
