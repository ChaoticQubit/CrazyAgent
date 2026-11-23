use super::*;
use crate::install::{InstallRecipe, SystemManager, TEMPLATE_MISMATCH, Verb};

const HASH: &str = "743f85035a1996ecb350659e01cd9faea9709829be09b1b4b03c73eaf3c9cd73";

fn words(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

fn at(path: &str) -> Option<PathBuf> {
    Some(PathBuf::from(path))
}

fn program(path: &str) -> InstallPaths {
    InstallPaths {
        program: at(path),
        ..InstallPaths::default()
    }
}

fn download(member: Option<&str>) -> InstallPaths {
    InstallPaths {
        archive: at("/s/stage/archive.tar.gz"),
        out: at("/s/stage/out"),
        source: Some("https://example.test/uv.tar.gz".to_owned()),
        member: member.map(str::to_owned),
        ..InstallPaths::default()
    }
}

fn uv_release() -> InstallRecipe {
    InstallRecipe::UvRelease {
        version: "0.13.5".to_owned(),
        target: "aarch64-apple-darwin".to_owned(),
        sha256: HASH.to_owned(),
    }
}

fn npm(version: &str, flags: &[&str]) -> InstallRecipe {
    InstallRecipe::NpmGlobal {
        package: "@openai/codex".to_owned(),
        version: version.to_owned(),
        verb: Verb::Update,
        flags: flags.iter().map(|flag| (*flag).to_owned()).collect(),
    }
}

fn npm_paths() -> InstallPaths {
    InstallPaths {
        program: at("/e/setup/bin/npm"),
        npm_prefix: at("/e/setup/npm"),
        ..InstallPaths::default()
    }
}

/// The two curl and tar words of today's download, `setup/pinned.rs` `download` :280-292
/// and `unpack_args` :499-506 with the member pushed after them at :247.
fn curl_and_tar(member: Option<&str>) -> Vec<Step> {
    let mut unpack = words(&[
        "--no-same-owner",
        "--no-same-permissions",
        "-xzf",
        "/s/stage/archive.tar.gz",
        "-C",
        "/s/stage/out",
    ]);
    unpack.extend(member.map(OsString::from));
    vec![
        Step {
            program: PathBuf::from("/usr/bin/curl"),
            args: words(&[
                "-q",
                "-fsSL",
                "--proto",
                "=https,file",
                "--max-filesize",
                "134217728",
                "--max-time",
                "300",
                "-o",
                "/s/stage/archive.tar.gz",
                "https://example.test/uv.tar.gz",
            ]),
            env: Vec::new(),
        },
        Step {
            program: PathBuf::from("/usr/bin/tar"),
            args: unpack,
            env: Vec::new(),
        },
    ]
}

/// A uv release runs today's curl, then today's tar with the archive's `uv` member.
#[test]
fn uv_release_runs_todays_argument_vector() {
    assert_eq!(
        steps(&uv_release(), &download(Some("uv-aarch64-apple-darwin/uv"))),
        Ok(curl_and_tar(Some("uv-aarch64-apple-darwin/uv")))
    );
}

/// A Node archive runs today's curl, then today's tar with no member, as
/// `setup/node.rs` `install_pin` :233-239 does with `unpack_args` at :235.
#[test]
fn node_archive_runs_todays_argument_vector() {
    let recipe = InstallRecipe::NodeArchive {
        version: "26.13.1".to_owned(),
        target: "aarch64-apple-darwin".to_owned(),
        sha256: HASH.to_owned(),
    };
    assert_eq!(steps(&recipe, &download(None)), Ok(curl_and_tar(None)));
}

/// A locked uv tool runs today's two uv commands, `setup.rs` `locked_tool_args` :395-423.
#[test]
fn uv_tool_runs_todays_argument_vector() {
    let recipe = InstallRecipe::UvTool {
        package: "graphifyy".to_owned(),
        version: "0.4.21".to_owned(),
        python: "3.12".to_owned(),
        lock: "x".to_owned(),
    };
    let paths = InstallPaths {
        program: at(if cfg!(windows) { "/e/uv.exe" } else { "/e/uv" }),
        venv: at("/e/env/venv"),
        venv_python: at("/e/env/venv/bin/python"),
        requirements: at("/e/env/requirements.txt"),
        ..InstallPaths::default()
    };
    let uv = paths.program.clone().unwrap();
    assert_eq!(
        steps(&recipe, &paths),
        Ok(vec![
            Step {
                program: uv.clone(),
                args: words(&[
                    "venv",
                    "--no-project",
                    "--no-config",
                    "--managed-python",
                    "--python",
                    "3.12",
                    "/e/env/venv",
                ]),
                env: Vec::new(),
            },
            Step {
                program: uv,
                args: words(&[
                    "pip",
                    "install",
                    "--no-config",
                    "--python",
                    "/e/env/venv/bin/python",
                    "--require-hashes",
                    "--no-build",
                    "--requirements",
                    "/e/env/requirements.txt",
                ]),
                env: Vec::new(),
            },
        ])
    );
}

/// A formula runs today's `brew upgrade --formula <name>`, `harnesses/update.rs` :613, and
/// its install form `brew install --formula <name>`, :671 and `setup/linux.rs` :77.
#[test]
fn brew_formula_runs_todays_argument_vector() {
    for (verb, word) in [(Verb::Update, "upgrade"), (Verb::Install, "install")] {
        let recipe = InstallRecipe::BrewFormula {
            name: "openclaw-cli".to_owned(),
            verb,
        };
        assert_eq!(
            steps(&recipe, &program("/opt/homebrew/bin/brew")),
            Ok(vec![Step {
                program: PathBuf::from("/opt/homebrew/bin/brew"),
                args: words(&[word, "--formula", "openclaw-cli"]),
                env: Vec::new(),
            }])
        );
    }
}

/// A cask runs today's `brew upgrade --cask <name>`, `harnesses/update.rs` :613 with
/// `flag` :270-275.
#[test]
fn brew_cask_runs_todays_argument_vector() {
    let recipe = InstallRecipe::BrewCask {
        name: "codex".to_owned(),
        verb: Verb::Update,
    };
    assert_eq!(
        steps(&recipe, &program("/opt/homebrew/bin/brew")),
        Ok(vec![Step {
            program: PathBuf::from("/opt/homebrew/bin/brew"),
            args: words(&["upgrade", "--cask", "codex"]),
            env: Vec::new(),
        }])
    );
}

/// An npm package runs today's `npm install -g <flags> <spec>` with npm's global prefix in
/// `npm_config_prefix`, `harnesses/update.rs` `npm_install_args` :455-460 and
/// `harnesses/update/run.rs` :266-268.
#[test]
fn npm_global_runs_todays_argument_vector() {
    assert_eq!(
        steps(&npm("latest", &["--ignore-scripts"]), &npm_paths()),
        Ok(vec![Step {
            program: PathBuf::from("/e/setup/bin/npm"),
            args: words(&["install", "-g", "--ignore-scripts", "@openai/codex@latest"]),
            env: vec![("npm_config_prefix", OsString::from("/e/setup/npm"))],
        }])
    );
}

/// A bare manifest spec such as `@openai/codex` is an npm recipe with an empty version,
/// and it runs the package name alone.
#[test]
fn npm_global_with_no_version_runs_the_bare_spec() {
    assert_eq!(
        steps(&npm("", &[]), &npm_paths()).map(|steps| steps[0].args.clone()),
        Ok(words(&["install", "-g", "@openai/codex"]))
    );
}

/// A Winget id runs today's user-scope silent install, `setup/windows.rs` `uv_command_on`
/// :190-206.
#[test]
fn winget_runs_todays_argument_vector() {
    let recipe = InstallRecipe::Winget {
        id: "astral-sh.uv".to_owned(),
        version: "0.13.5".to_owned(),
    };
    let winget = "/w/WindowsApps/winget.exe";
    assert_eq!(
        steps(&recipe, &program(winget)),
        Ok(vec![Step {
            program: PathBuf::from(winget),
            args: words(&[
                "install",
                "--id",
                "astral-sh.uv",
                "--exact",
                "--version",
                "0.13.5",
                "--source",
                "winget",
                "--scope",
                "user",
                "--silent",
                "--disable-interactivity",
                "--accept-package-agreements",
                "--accept-source-agreements",
            ]),
            env: Vec::new(),
        }])
    );
}

/// A system package runs today's apt-get or dnf words, `setup/linux.rs` `tmux_command`
/// :84-94, apt-get with `DEBIAN_FRONTEND=noninteractive`.
#[test]
fn system_package_runs_todays_argument_vector() {
    let apt = InstallRecipe::SystemPackage {
        manager: SystemManager::AptGet,
        package: "tmux".to_owned(),
    };
    assert_eq!(
        steps(&apt, &program("/usr/bin/apt-get")),
        Ok(vec![Step {
            program: PathBuf::from("/usr/bin/apt-get"),
            args: words(&["install", "-y", "--no-install-recommends", "tmux"]),
            env: vec![("DEBIAN_FRONTEND", OsString::from("noninteractive"))],
        }])
    );
    let dnf = InstallRecipe::SystemPackage {
        manager: SystemManager::Dnf,
        package: "tmux".to_owned(),
    };
    assert_eq!(
        steps(&dnf, &program("/usr/bin/dnf")),
        Ok(vec![Step {
            program: PathBuf::from("/usr/bin/dnf"),
            args: words(&["install", "-y", "tmux"]),
            env: Vec::new(),
        }])
    );
}

/// A self-update runs the harness entry with its bundled words, `harnesses/update.rs`
/// :598-608.
#[test]
fn self_update_runs_todays_argument_vector() {
    let recipe = InstallRecipe::SelfUpdate {
        program: "codex".to_owned(),
        args: vec!["update".to_owned()],
    };
    assert_eq!(
        steps(&recipe, &program("/u/.local/bin/codex")),
        Ok(vec![Step {
            program: PathBuf::from("/u/.local/bin/codex"),
            args: words(&["update"]),
            env: Vec::new(),
        }])
    );
}

/// An installer program whose name is not the variant's installer is refused, so an npm
/// recipe never runs another program.
#[test]
fn a_program_that_is_not_the_variants_installer_is_refused() {
    let paths = InstallPaths {
        program: at("/e/setup/bin/node"),
        ..npm_paths()
    };
    assert_eq!(steps(&npm("", &[]), &paths), Err(TEMPLATE_MISMATCH));
    let recipe = InstallRecipe::SelfUpdate {
        program: "codex".to_owned(),
        args: vec!["update".to_owned()],
    };
    assert_eq!(
        steps(&recipe, &program("/u/.local/bin/claude")),
        Err(TEMPLATE_MISMATCH)
    );
}

/// A path the variant needs left unset, and one it does not use set, are each refused.
#[test]
fn a_needed_path_left_unset_or_an_unused_one_set_is_refused() {
    let missing = InstallPaths {
        npm_prefix: None,
        ..npm_paths()
    };
    assert_eq!(steps(&npm("", &[]), &missing), Err(TEMPLATE_MISMATCH));
    let extra = InstallPaths {
        venv: at("/e/env/venv"),
        ..npm_paths()
    };
    assert_eq!(steps(&npm("", &[]), &extra), Err(TEMPLATE_MISMATCH));
    let member = download(Some("node/bin/node"));
    let node = InstallRecipe::NodeArchive {
        version: "26.13.1".to_owned(),
        target: "aarch64-apple-darwin".to_owned(),
        sha256: HASH.to_owned(),
    };
    assert_eq!(steps(&node, &member), Err(TEMPLATE_MISMATCH));
}

/// A recipe validation refuses is refused before any step is built.
#[test]
fn an_invalid_recipe_builds_no_step() {
    assert_eq!(
        steps(&npm("-1", &[]), &npm_paths()),
        Err(crate::install::VERSION_INVALID)
    );
}

/// A source that is not an https or local file address, past its bound or holding a space,
/// and a member that leaves the archive folder, starts with `-` or is not `uv`, are refused.
#[test]
fn a_bad_download_source_or_member_is_refused() {
    for source in [
        "http://example.test/uv.tar.gz",
        "file://host/uv.tar.gz",
        "https://example.test/uv tar.gz",
        &format!("https://{}", "a".repeat(SOURCE_BYTES_MAX)),
    ] {
        let paths = InstallPaths {
            source: Some(source.to_owned()),
            ..download(Some("uv-aarch64-apple-darwin/uv"))
        };
        assert_eq!(
            steps(&uv_release(), &paths),
            Err(TEMPLATE_MISMATCH),
            "{source}"
        );
    }
    for member in ["../uv", "/uv", "-uv/uv", "uv-aarch64-apple-darwin/uvx"] {
        assert_eq!(
            steps(&uv_release(), &download(Some(member))),
            Err(TEMPLATE_MISMATCH),
            "{member}"
        );
    }
}

/// A relative engine path is an engine fault, never a recipe the kernel builds on.
#[test]
#[should_panic(expected = "absolute")]
fn a_relative_engine_path_is_never_built_on() {
    let _ = steps(
        &npm("", &[]),
        &InstallPaths {
            npm_prefix: at("setup/npm"),
            ..npm_paths()
        },
    );
}

/// One step runs with only the base and step environment, and reports its exit as 0 or 1
/// with its standard output.
#[cfg(unix)]
#[test]
fn a_step_runs_with_only_its_environment() {
    let step = Step {
        program: PathBuf::from("/bin/sh"),
        args: words(&["-c", "printf '%s %s' \"$BASE\" \"$STEP\"; [ -z \"$HOME\" ]"]),
        env: vec![("STEP", OsString::from("two"))],
    };
    let folder = std::env::temp_dir();
    let ran = run(
        &step,
        &[("BASE", OsString::from("one"))],
        &folder,
        std::time::Duration::from_secs(10),
    );
    assert_eq!(
        ran,
        Ok(InstallResult {
            code: 0,
            output: b"one two".to_vec(),
        })
    );
}
