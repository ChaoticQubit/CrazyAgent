use super::stub::*;
use super::*;

use std::os::unix::fs::PermissionsExt;

/// A scratch HOME with no tools in it, its real path, so found files compare equal.
fn scratch() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::Builder::new()
        .prefix("tools-resolve-")
        .tempdir()
        .unwrap();
    let home = folder.path().canonicalize().unwrap();
    (folder, home)
}

/// Writes an executable file with the given text, its folders made first.
/// Inputs: the file and the text. Output: the file.
fn program(file: &Path, text: &str) -> PathBuf {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
    file.to_path_buf()
}

/// A stand-in `node`: a shell script that says it ran the file it was given.
const NODE: &str = "#!/bin/sh\necho \"node ran $1\"\n";

/// The places the engine builds for a scratch HOME, kept to the folders inside it, so a
/// tool installed on this computer never answers for the scratch one. Input: HOME.
/// Output: the places.
fn home_only(home: &Path) -> Places {
    let engine = stub_engine(None, None, Some(home.to_path_buf()), Some(OsStr::new("")));
    stub_new(
        engine
            .folders()
            .iter()
            .filter(|folder| folder.starts_with(home))
            .cloned()
            .collect(),
        Some(home.to_path_buf()),
    )
}

/// The newest of several installed Node versions is found first.
#[test]
fn the_newest_node_version_of_a_manager_is_found_first() {
    let (_folder, home) = scratch();
    program(&home.join(".volta/tools/image/node/18.20.0/bin/node"), NODE);
    let newest = program(&home.join(".volta/tools/image/node/22.11.0/bin/node"), NODE);
    program(&home.join(".volta/tools/image/node/20.18.0/bin/node"), NODE);
    assert_eq!(resolve("node", &home_only(&home)).unwrap().program, newest);
}

/// Writes a script with the given bytes and mode into the scratch HOME's `scripts` folder.
/// Inputs: HOME, the script's name, its bytes and its mode. Output: the script.
fn script(home: &Path, name: &str, bytes: &[u8], mode: u32) -> PathBuf {
    let script = home.join("scripts").join(name);
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, bytes).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(mode)).unwrap();
    script
}

/// The file the resolver refused for an entry and why, none when it resolved or failed for
/// another reason. Inputs: the entry and the places. Output: the refusal.
fn refusal_of(entry: &Path, places: &Places) -> Option<(PathBuf, Refusal)> {
    resolve_entry(entry, places)
        .err()
        .filter(|missing| missing.reason == Reason::Refused)
        .and_then(|missing| missing.refused)
        .map(|refused| (refused.file, refused.why))
}

/// rv-sec2 F2 (B11): a script whose first line the resolver cannot read the way the system and
/// `env` run it is refused, never taken for a program with no interpreter: `env` with a
/// setting or a flag other than -S, an `env` other than /usr/bin/env, a relative interpreter,
/// one with a `..` part, a line that is not UTF-8, a line longer than the part read, and
/// `/usr/bin/env` reached as the program `env` runs or through a link of another name, plus
/// a CRLF line end, more words than the reader keeps and an `-S` program holding `$`
/// (B11 r2 N1), each as [`Refusal::FirstLine`].
/// `env -S` still resolves (`shebang_flags_and_absolute_interpreters_are_kept`).
#[test]
fn a_first_line_the_resolver_cannot_read_as_the_system_runs_it_is_refused() {
    let (_folder, home) = scratch();
    program(&home.join(".local/bin/node"), NODE);
    std::os::unix::fs::symlink("/usr/bin/env", home.join(".local/bin/e")).unwrap();
    let places = home_only(&home);
    let lines: Vec<(&str, Vec<u8>)> = vec![
        ("crlf", b"#!/bin/sh\r\n".to_vec()),
        ("wordy", b"#!/bin/sh a b c d e f g h\n".to_vec()),
        ("dollar", b"#!/usr/bin/env -S no$de\n".to_vec()),
        ("assigned", b"#!/usr/bin/env PATH=/tmp/x node\n".to_vec()),
        ("flagged", b"#!/usr/bin/env -i node\n".to_vec()),
        (
            "other-env",
            format!("#!{}/env node\n", home.join(".local/bin").display()).into_bytes(),
        ),
        ("relative", b"#!node\n".to_vec()),
        ("env-twice", b"#!/usr/bin/env /usr/bin/env node\n".to_vec()),
        (
            "env-link",
            format!("#!{}/e node\n", home.join(".local/bin").display()).into_bytes(),
        ),
        ("dotted", b"#!/usr/bin/../bin/sh\n".to_vec()),
        ("not-utf8", b"#!/bin/\xffsh\n".to_vec()),
        (
            "long",
            format!(
                "#!/usr/bin/env -S{}node\n",
                " ".repeat(SCRIPT_HEAD_BYTES_MAX)
            )
            .into_bytes(),
        ),
    ];
    for (name, line) in lines {
        let script = script(&home, name, &line, 0o755);
        assert_eq!(
            refusal_of(&script, &places),
            Some((script, Refusal::FirstLine)),
            "{name}"
        );
    }
}

/// rv-sec2 B11 r2 F3: the system ends a first line at its first `#`, so the words the reader
/// would keep past it are not the ones that run: a line holding `#` anywhere after `#!` is
/// refused as [`Refusal::FirstLine`], an `env` program named `node#x` with an owner file of
/// that name in the places included.
#[test]
fn a_first_line_holding_a_hash_is_refused() {
    let (_folder, home) = scratch();
    program(&home.join(".local/bin/node"), NODE);
    program(&home.join(".local/bin/node#x"), NODE);
    let places = home_only(&home);
    let lines: Vec<(&str, &[u8])> = vec![
        ("word", &b"#!/bin/sh a#b c\n"[..]),
        ("named", &b"#!/usr/bin/env node#x\n"[..]),
        ("spaced", &b"#!/bin/sh # c\n"[..]),
    ];
    for (name, line) in lines {
        let script = script(&home, name, line, 0o755);
        assert_eq!(
            refusal_of(&script, &places),
            Some((script, Refusal::FirstLine)),
            "{name}"
        );
    }
}

/// rv-sec2 B11 r2 F4: the system reads a script's first line whatever its read bit says, so an
/// executable file the engine cannot read is refused as [`Refusal::FirstLine`], never taken
/// for a program with no interpreter; the same file readable resolves.
#[test]
fn an_executable_file_the_engine_cannot_read_is_refused() {
    let (_folder, home) = scratch();
    let places = home_only(&home);
    let locked = script(&home, "locked", b"#!/bin/echo ran\n", 0o311);
    assert_eq!(
        refusal_of(&locked, &places),
        Some((locked.clone(), Refusal::FirstLine))
    );
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(resolve_entry(&locked, &places).is_ok());
}

/// V243-OWN-PREFIX-NODE-REFUSE trip row: [`own_node_refused`] holds only a tool whose real
/// file is absolute, as every tool [`entry_at`] gives is.
#[test]
#[should_panic(expected = "assertion failed: tool.program.is_absolute()")]
fn the_own_prefix_node_check_trips_on_a_tool_whose_file_is_relative() {
    let (_folder, home) = scratch();
    let tool = Resolved {
        entry: PathBuf::from("codex"),
        program: PathBuf::from("codex"),
        kind: Kind::Native,
    };
    let _ = own_node_refused(&tool, &home_only(&home));
}

/// V243-OWN-PREFIX-NODE-REFUSE trip row: [`own_node`] judges only a script named by its
/// absolute real file.
#[test]
#[should_panic(expected = "assertion failed: script.is_absolute()")]
fn the_own_prefix_node_trips_on_a_relative_script() {
    let (_folder, home) = scratch();
    let _ = own_node(
        Path::new("lib/node_modules/pkg/cli.js"),
        &home_only(&home),
        1,
    );
}

/// V243-OWN-PREFIX-NODE-REFUSE trip row: [`own_node`] never judges a `node` past
/// [`INTERPRETER_DEPTH_MAX`].
#[test]
#[should_panic(expected = "assertion failed: depth <= INTERPRETER_DEPTH_MAX")]
fn the_own_prefix_node_trips_past_the_interpreter_depth() {
    let (_folder, home) = scratch();
    let script = home.join("nvm-like/lib/node_modules/pkg/cli.js");
    let _ = own_node(&script, &home_only(&home), INTERPRETER_DEPTH_MAX + 1);
}

/// R-M1: the engine's data folder holds the agents' HOMEs, worktrees and team state, which
/// agents write. A PATH folder there is refused while the engine's own `setup/bin` is kept,
/// and a file found in `setup/bin` whose real file sits in an agent's HOME is passed over.
#[test]
fn a_path_folder_in_the_engine_data_folder_is_refused_and_its_setup_folder_is_kept() {
    let (_folder, home) = scratch();
    let root = home.join("engine");
    let setup = root.join("setup/bin");
    let agent = root.join("team/agents/agent-1/bin");
    let path = std::env::join_paths([&agent, &setup]).unwrap();
    let engine = stub_engine(Some(&root), None, Some(home.clone()), Some(&path));
    assert!(engine.folders().contains(&setup), "{:?}", engine.folders());
    assert!(!engine.folders().contains(&agent), "{:?}", engine.folders());

    let planted = program(&agent.join("tmux"), NODE);
    std::fs::create_dir_all(&setup).unwrap();
    std::os::unix::fs::symlink(&planted, setup.join("tmux")).unwrap();
    let places = stub_engine(Some(&root), None, Some(home.clone()), Some(OsStr::new("")));
    assert!(
        resolve("tmux", &places).map_or(true, |tmux| tmux.program != planted),
        "a link into an agent's HOME is never used"
    );
    assert!(resolve_entry(&setup.join("tmux"), &places).is_err());
    std::fs::remove_file(setup.join("tmux")).unwrap();
    let own = program(&setup.join("tmux"), NODE);
    assert_eq!(
        resolve_entry(&setup.join("tmux"), &places).unwrap().program,
        own,
        "the setup folder is kept, though an owner tmux found first wins by name"
    );
}

/// R-M1: a folder of the PATH the engine inherited, frozen from the shell that installed it,
/// that sits in a temp folder, which agents with the temp folder grant and other users
/// write, is refused. Probed folders meet the same rule on the engine's places (2.4.1 R1).
#[test]
fn a_path_folder_in_a_temp_folder_is_refused() {
    let (_folder, home) = scratch();
    let owner_temp = std::env::temp_dir().join("tools-planted/bin");
    let shared = PathBuf::from("/private/tmp/tools-planted/bin");
    let path = std::env::join_paths([&owner_temp, &shared]).unwrap();
    let engine = stub_engine(None, None, Some(home.clone()), Some(&path));
    assert!(
        !engine.folders().contains(&owner_temp),
        "{:?}",
        engine.folders()
    );
    assert!(
        !engine.folders().contains(&shared),
        "{:?}",
        engine.folders()
    );
}

/// 2.4.1 R1: a folder the probes found, a login shell PATH folder or npm's global prefix,
/// that sits in this user's temp folder or a shared one is refused on the engine's places,
/// as written and by its real path: agents with the owner's temp folder grant write the one
/// and other users the other, and the engine runs what it finds there outside any sandbox.
#[test]
fn a_probed_folder_in_a_temp_folder_is_refused_on_the_engine_places() {
    let (_folder, home) = scratch();
    let owner_temp = tempfile::Builder::new()
        .prefix("tools-probed-")
        .tempdir()
        .unwrap();
    let written = owner_temp.path().join("bin");
    std::fs::create_dir_all(&written).unwrap();
    let real = written.canonicalize().unwrap();
    let shared = PathBuf::from("/private/tmp/tools-planted/bin");
    let engine = stub_engine(None, None, Some(home), Some(OsStr::new("")));
    let probed = engine.with_probed(vec![written.clone(), real.clone(), shared.clone()]);
    for folder in [&written, &real, &shared] {
        assert!(
            !probed.folders().contains(folder),
            "{folder:?} in {:?}",
            probed.folders()
        );
    }
}

/// 2.4.1 r2 RH2-L1: a file found through a place outside every temp folder whose real file
/// lies in one, such as an owner link in `~/.local/bin` into the temp folder, leads into
/// temp; one whose place is itself in a temp folder, or whose real file is not, does not.
#[test]
fn a_link_from_a_place_outside_temp_into_a_temp_folder_leads_into_temp() {
    let temps = [
        PathBuf::from("/private/tmp"),
        PathBuf::from("/private/var/folders/xy/T"),
    ];
    let link = Path::new("/Users/ana/.local/bin/tool");
    assert!(leads_into_temp(
        &STUB_HOOKS,
        link,
        Path::new("/private/var/folders/xy/T/x/tool"),
        &temps
    ));
    assert!(leads_into_temp(
        &STUB_HOOKS,
        link,
        Path::new("/private/tmp/x/tool"),
        &temps
    ));
    assert!(!leads_into_temp(
        &STUB_HOOKS,
        link,
        Path::new("/opt/tools/tool"),
        &temps
    ));
    assert!(
        !leads_into_temp(
            &STUB_HOOKS,
            Path::new("/private/tmp/home/.local/bin/tool"),
            Path::new("/private/tmp/other/tool"),
            &temps
        ),
        "a place itself in a temp folder"
    );
    assert!(!leads_into_temp(
        &STUB_HOOKS,
        Path::new("/private/var/folders/xy/T/home/bin/tool"),
        Path::new("/private/tmp/x/tool"),
        &temps
    ));
}

/// A tool in this user's own folder under `/tmp` (root 1777, sticky) is used: no other user
/// can replace a folder this user owns there, so test stand-ins and engines started from a
/// `/tmp` folder resolve.
#[test]
fn a_tool_in_an_own_folder_under_the_shared_temp_folder_is_used() {
    let folder = tempfile::Builder::new()
        .prefix("tools-tmp-")
        .tempdir_in("/tmp")
        .unwrap();
    let own = folder.path().canonicalize().unwrap();
    let tool = program(&own.join("tool"), NODE);
    let places = stub_new(vec![own.clone()], None);
    assert_eq!(resolve("tool", &places).unwrap().program, tool);
}

/// The app's folder as a release lays it out, holding the engine and the bundled
/// `crazyagent-tmux` beside it (DEC-157), plus a Homebrew-like folder with its own `tmux`.
/// The places look only in that folder, past the app's folder, so no row reads the host's
/// own tmux. Input: HOME. Output: the places, the sidecar and the Homebrew-like tmux.
fn bundled_layout(home: &Path) -> (Places, PathBuf, PathBuf) {
    let app = home.join("Applications/CrazyAgent.app/Contents/MacOS");
    program(&app.join("crazyagent"), "#!/bin/sh\n");
    let sidecar = program(&app.join("crazyagent-tmux"), "#!/bin/sh\necho tmux 3.8\n");
    let brew = program(
        &home.join("opt/homebrew/bin/tmux"),
        "#!/bin/sh\necho tmux 3.5a\n",
    );
    let places = Places {
        app_folder: Some(app),
        ..stub_new(
            vec![brew.parent().unwrap().to_path_buf()],
            Some(home.to_path_buf()),
        )
    };
    assert_eq!(places.folders().len(), 1);
    (places, sidecar, brew)
}

/// The engine uses the tmux it ships. The name `tmux` finds
/// `<app folder>/crazyagent-tmux` before Homebrew or anything on PATH, read as any entry is,
/// and its words and the sandbox grants start from that file, never the Homebrew one.
#[test]
fn the_bundled_tmux_beside_the_engine_wins_over_a_homebrew_tmux() {
    let (_folder, home) = scratch();
    let (places, sidecar, brew) = bundled_layout(&home);
    let tmux = resolve("tmux", &places).unwrap();
    assert_eq!(tmux.entry, sidecar);
    assert_eq!(tmux.program, sidecar.canonicalize().unwrap());
    assert_eq!(tmux.argv(), vec![sidecar.clone().into_os_string()]);
    let grants = tmux.grants();
    assert_eq!(grants[0], sidecar);
    assert!(
        !grants.contains(&brew),
        "the Homebrew tmux is never granted"
    );
}

/// The engine adds the probed folders to its places before any launch
/// ([`Places::with_probed`]), which rebuilds them; the bundled tmux stays first through that
/// step, so the shipped engine and the sandbox grant use it, not only places built by hand.
#[test]
fn the_bundled_tmux_stays_first_once_the_probed_folders_are_added() {
    let (_folder, home) = scratch();
    let (_places, sidecar, brew) = bundled_layout(&home);
    let app = sidecar.parent().unwrap();
    let engine = stub_engine(None, Some(app), Some(home.clone()), None);
    let probed = engine.with_probed(vec![
        brew.parent().unwrap().to_path_buf(),
        home.join("probed/bin"),
    ]);
    assert_eq!(probed.bundled("tmux"), Some(sidecar.clone()));
    assert_eq!(resolve("tmux", &probed).unwrap().entry, sidecar);
}

/// With no bundled tmux beside the engine, such as a 2.4.0 install or a dev build, the name
/// `tmux` finds what the usual search order finds, here the Homebrew-like one, and no other
/// tool name ever gets the bundled file.
#[test]
fn a_missing_bundled_tmux_falls_back_to_the_usual_search_order() {
    let (_folder, home) = scratch();
    let (places, sidecar, brew) = bundled_layout(&home);
    std::fs::remove_file(&sidecar).unwrap();
    assert_eq!(resolve("tmux", &places).unwrap().entry, brew);
    let (_other, home) = scratch();
    let (places, sidecar, _brew) = bundled_layout(&home);
    for name in ["uv", "node", "screen"] {
        assert!(
            !matches!(resolve(name, &places), Ok(found) if found.entry == sidecar),
            "{name} never resolves to the bundled tmux"
        );
    }
}

/// Leader 20:08: a bundled tmux whose version read fails is passed over for the usual order,
/// which says the bundled one can't run; only the bundled copy is ever asked whether it runs.
#[test]
fn a_bundled_tmux_that_cannot_run_falls_back_to_the_usual_order_and_says_so() {
    let (_folder, home) = scratch();
    let (places, sidecar, brew) = bundled_layout(&home);
    let asked = std::cell::RefCell::new(Vec::new());
    let (found, note) = resolve_tmux(&places, true, |tmux| {
        asked.borrow_mut().push(tmux.entry.clone());
        false
    })
    .unwrap();
    assert_eq!(found.entry, brew);
    assert_eq!(note, Some(BundleNote::CannotRun));
    assert_eq!(asked.into_inner(), vec![sidecar]);
}

/// A bundled tmux that runs is the one used, with no note, whether or not the build is
/// marked as shipping one.
#[test]
fn a_bundled_tmux_that_runs_is_used_with_no_note() {
    let (_folder, home) = scratch();
    let (places, sidecar, _brew) = bundled_layout(&home);
    for ships in [true, false] {
        let (found, note) = resolve_tmux(&places, ships, |_| true).unwrap();
        assert_eq!(found.entry, sidecar);
        assert_eq!(note, None);
    }
}

/// A build that ships tmux and has lost it uses the usual order and says the bundled one is
/// missing; a build that ships none, such as a dev build, says nothing.
#[test]
fn a_lost_bundled_tmux_says_it_is_missing_only_in_a_build_that_ships_one() {
    let (_folder, home) = scratch();
    let (places, sidecar, brew) = bundled_layout(&home);
    std::fs::remove_file(&sidecar).unwrap();
    let never = |_: &Resolved| -> bool { panic!("nothing to run") };
    assert_eq!(
        resolve_tmux(&places, true, never).unwrap(),
        (resolve("tmux", &places).unwrap(), Some(BundleNote::Missing))
    );
    let (found, note) = resolve_tmux(&places, false, never).unwrap();
    assert_eq!((found.entry, note), (brew, None));
}

/// The two notes, word for word as the TUI and the desktop app show them.
#[test]
fn the_bundle_notes_name_the_system_tmux_and_the_reinstall() {
    assert_eq!(
        BundleNote::Missing.line(),
        "Using the system tmux, the bundled one is missing; reinstall CrazyAgent."
    );
    assert_eq!(
        BundleNote::CannotRun.line(),
        "Using the system tmux, the bundled one can't run; reinstall CrazyAgent."
    );
}

/// Clean-Mac gate shape d: under the gate's stand-in system root, a folder the login shell
/// probe found outside that root and HOME, such as this computer's real Homebrew, is never
/// looked in, while the gate's own Homebrew and HOME folders are; a release, with no gate
/// root, keeps every probed folder as before.
#[test]
fn under_the_gate_root_a_probed_folder_outside_the_gate_mac_is_dropped() {
    let (_folder, run) = scratch();
    let home = run.join("home");
    let root = run.join("system");
    let mapped = root.join("opt/homebrew/bin");
    let npm = home.join(".npm-global/bin");
    let host = PathBuf::from("/opt/homebrew/bin");
    let probed = vec![host.clone(), mapped.clone(), npm.clone()];
    assert_eq!(
        probed_kept(probed.clone(), Some(&root), Some(&home)),
        vec![mapped, npm]
    );
    assert_eq!(probed_kept(probed.clone(), None, Some(&home)), probed);
}

#[test]
fn places_that_differ_in_any_field_but_the_hooks_compare_unequal() {
    let home = tempfile::tempdir().unwrap();
    let base = stub_new(
        vec![PathBuf::from("/usr/bin")],
        Some(home.path().to_path_buf()),
    );
    assert!(base.home.is_some() && base.projects.is_some());
    let path = || vec![PathBuf::from("/opt/other")];
    let changed = [
        Places {
            folders: path(),
            ..base.clone()
        },
        Places {
            home: None,
            ..base.clone()
        },
        Places {
            engine_roots: path(),
            ..base.clone()
        },
        Places {
            temp_refused: !base.temp_refused,
            ..base.clone()
        },
        Places {
            temps: path(),
            ..base.clone()
        },
        Places {
            refused_setup: path(),
            ..base.clone()
        },
        Places {
            refused_temp: path(),
            ..base.clone()
        },
        Places {
            app_folder: Some(PathBuf::from("/opt/app")),
            ..base.clone()
        },
        Places {
            projects: None,
            ..base.clone()
        },
        Places {
            seed: FindingSeed {
                layout_root: Some(PathBuf::from("/opt/data")),
                ..base.seed.clone()
            },
            ..base.clone()
        },
        Places {
            seed: FindingSeed {
                harness_locations: vec![".other/bin"],
                ..base.seed.clone()
            },
            ..base.clone()
        },
    ];
    assert_eq!(base, base.clone());
    for (index, places) in changed.iter().enumerate() {
        assert_ne!(
            &base, places,
            "field {index} must take part in the comparison"
        );
    }
}

#[test]
fn the_release_build_sees_the_bundled_tmux_flag() {
    let set = std::env::var_os("CRAZYAGENT_BUNDLES_TMUX");
    let expected = set.as_deref() == Some(std::ffi::OsStr::new("1"));
    assert_eq!(
        SHIPS_BUNDLED_TMUX, expected,
        "the kernel crate reads CRAZYAGENT_BUNDLES_TMUX={set:?} at build time"
    );
}

#[test]
fn release_yml_sets_the_flag_for_the_kernel_crate() {
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.github/workflows/release.yml"),
    )
    .unwrap();
    let build = "cargo build --release --locked --target \"$TARGET\" -p crazyagent --bins";
    let build_at = text
        .find(build)
        .expect("the release job builds the engine binaries");
    let job_at = text[..build_at]
        .rfind("\n  build:\n")
        .expect("the engine build step sits in the build job");
    let job = &text[job_at..build_at];
    let env_at = job
        .find("\n    env:\n")
        .expect("the job sets its environment for every step");
    let steps_at = job.find("\n    steps:\n").expect("the job lists its steps");
    assert!(env_at < steps_at, "the job env comes before its steps");
    assert!(
        job[env_at..steps_at].lines().any(|line| line
            == "      CRAZYAGENT_BUNDLES_TMUX: ${{ matrix.os != 'windows' && '1' || '0' }}"),
        "the flag is set for the whole job, so every crate the build compiles sees it"
    );
}

/// A gate root hook that names none remaps nothing: Homebrew's folders are the real ones and
/// a probed folder outside HOME is kept. A hook that names a root, the positive control,
/// moves Homebrew under it and drops that folder.
#[test]
fn a_gate_root_hook_that_names_none_remaps_nothing() {
    let (_home_folder, home) = scratch();
    let (_outside_folder, outside) = scratch();
    assert_eq!(gate_root(&STUB_HOOKS), None);
    assert_eq!(
        homebrew_folders(&STUB_HOOKS),
        HOMEBREW_FOLDERS.map(PathBuf::from)
    );
    let open = stub_new(Vec::new(), Some(home.clone())).with_probed(vec![outside.clone()]);
    assert!(open.folders().contains(&outside), "{:?}", open.folders());

    let gated = FindingHooks {
        gate_root: || Some(PathBuf::from("/opt/gate")),
        ..STUB_HOOKS
    };
    assert_eq!(
        homebrew_folders(&gated),
        [
            PathBuf::from("/opt/gate/opt/homebrew/bin"),
            PathBuf::from("/opt/gate/usr/local/bin")
        ]
    );
    let seed = FindingSeed {
        hooks: gated,
        ..stub_seed()
    };
    let closed = Places::new(Vec::new(), Some(home), seed).with_probed(vec![outside.clone()]);
    assert!(
        !closed.folders().contains(&outside),
        "{:?}",
        closed.folders()
    );
}

/// Places on the stub seed with the canonical hook given, HOME in a scratch folder and the
/// set-up projects read as given. Inputs: HOME, the hook and the projects. Output: the places.
fn with_canonical(
    home: &Path,
    canonical: fn(&Path) -> std::io::Result<PathBuf>,
    projects: Option<Vec<PathBuf>>,
) -> Places {
    let mut seed = stub_seed();
    seed.hooks.canonical = canonical;
    Places {
        projects,
        ..Places::new(Vec::new(), Some(home.to_path_buf()), seed)
    }
}

/// A canonical hook that reads a Homebrew-style link as its `Cellar` file, and a link from
/// another prefix's `bin` as that prefix's `Cellar` file. Input: the path. Output: its one
/// spelling.
fn cellar(path: &Path) -> std::io::Result<PathBuf> {
    let text = path.to_str().unwrap();
    Ok(match text.rsplit_once("/bin/") {
        Some((prefix, name)) => PathBuf::from(format!("{prefix}/Cellar/{name}/1/bin/{name}")),
        None => path.to_path_buf(),
    })
}

/// A Homebrew tool, whose one spelling lies in its prefix's `Cellar`, stays usable
/// while the set-up projects cannot be read; a link from a prefix outside the install roots
/// whose every form lies there is agents' to write.
#[test]
fn a_cellar_linked_tool_stays_usable_while_the_projects_cannot_be_read() {
    let (_folder, home) = scratch();
    let places = with_canonical(&home, cellar, None);
    assert!(!places.agent_writable(Path::new("/opt/homebrew/bin/tmux")));
    assert!(places.agent_writable(Path::new("/opt/elsewhere/bin/tmux")));
}

/// The system's tools stay usable while the set-up projects cannot be read.
#[test]
fn a_system_tool_stays_usable_while_the_projects_cannot_be_read() {
    let (_folder, home) = scratch();
    let places = with_canonical(&home, |path| Ok(path.to_path_buf()), None);
    for tool in [
        "/usr/bin/env",
        "/bin/sh",
        "/usr/local/bin/tmux",
        "/sbin/mount",
    ] {
        assert!(!places.agent_writable(Path::new(tool)), "{tool}");
    }
}

/// The failed-read fallback never trusts more than a successful read with no project
/// does: every path agents can write with the projects read is agents' to write without
/// them, so the fallback only adds refusals.
#[test]
fn the_failed_read_fallback_never_trusts_a_path_the_normal_path_refuses() {
    let (_folder, home) = scratch();
    let marked = home.join("work/marked");
    std::fs::create_dir_all(marked.join(".crazyagent")).unwrap();
    let paths = [
        PathBuf::from("/usr/bin/env"),
        PathBuf::from("/usr/local/bin/tmux"),
        PathBuf::from("/opt/homebrew/bin/node"),
        PathBuf::from("/opt/homebrew/Cellar/node/1/bin/node"),
        PathBuf::from("/opt/elsewhere/bin/tool"),
        PathBuf::from("/srv/repo/bin/tool"),
        home.join("work/app/bin/tool"),
        marked.join("bin/tool"),
        home.join(".local/bin/tool"),
    ];
    let canonical: fn(&Path) -> std::io::Result<PathBuf> = |path| Ok(path.to_path_buf());
    let read = with_canonical(&home, canonical, Some(Vec::new()));
    let unread = with_canonical(&home, canonical, None);
    for path in &paths {
        if read.agent_writable(path) {
            assert!(unread.agent_writable(path), "{}", path.display());
        }
    }
    assert!(read.agent_writable(&marked.join("bin/tool")));
    assert!(!read.agent_writable(Path::new("/srv/repo/bin/tool")));
    assert!(unread.agent_writable(Path::new("/srv/repo/bin/tool")));
}

/// When the set-up projects cannot be read, a repository the engine may have
/// registered outside HOME still counts as agent-writable, so a tool in it is refused, while
/// a system folder's tool does not.
#[test]
fn a_registered_repository_outside_home_is_agent_writable_while_its_projects_cannot_be_read() {
    let (_folder, home) = scratch();
    let outside = tempfile::Builder::new()
        .prefix("tools-h8-repo-")
        .tempdir()
        .unwrap();
    let repo = outside.path().canonicalize().unwrap();
    assert!(!repo.starts_with(&home));
    let tool = program(&repo.join("bin/tool"), "#!/bin/sh\n");
    let places = Places {
        projects: None,
        ..stub_new(Vec::new(), Some(home.clone()))
    };
    assert!(places.agent_writable(&tool), "{}", tool.display());
    assert_eq!(
        check_place(&places, &tool),
        Some(Err(Refusal::AgentsCanWrite))
    );
    assert!(!places.agent_writable(Path::new("/bin/sh")));
}

/// A name whose first copy has a first line the resolver refuses resolves to a later copy
/// that runs, so one unreadable script never hides a working one further along the places.
#[test]
fn a_refused_first_copy_resolves_to_the_later_copy_that_runs() {
    let (_folder, home) = scratch();
    let first = home.join("first");
    let later = home.join("later");
    script_at(&first.join("tool"), b"#!/bin/sh\r\n");
    let runs = script_at(&later.join("tool"), b"#!/bin/sh\n");
    let places = stub_new(vec![first, later], Some(home.clone()));
    let resolved = resolve("tool", &places).unwrap();
    assert_eq!(resolved.entry, runs);
}

/// When no copy of a name resolves, the refusal reported is the first copy's, not a later
/// one's.
#[test]
fn the_first_copys_refusal_is_kept_when_no_copy_resolves() {
    let (_folder, home) = scratch();
    let first = home.join("first");
    let later = home.join("later");
    let refused = script_at(&first.join("tool"), b"#!/bin/sh\r\n");
    script_at(&later.join("tool"), b"#!node\n");
    let places = stub_new(vec![first, later], Some(home.clone()));
    let missing = resolve("tool", &places).unwrap_err();
    assert_eq!(missing.tool, "tool");
    let kept = missing.refused.expect("the first copy's refusal");
    assert_eq!((kept.file, kept.why), (refused, Refusal::FirstLine));
}

/// Writes an executable script with the given bytes, its folders made first. Inputs: the
/// file and the bytes. Output: the file.
fn script_at(file: &Path, bytes: &[u8]) -> PathBuf {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, bytes).unwrap();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
    file.to_path_buf()
}

/// A folder the engine named for agents under an install root still counts as one agents can
/// write while engine.db cannot be read, when an earlier read of the same data folder named
/// it: the failed read takes that data folder's last good read, so a tool planted there is
/// refused, and a data folder with no good read gains nothing from another one's.
#[test]
fn a_registered_folder_under_an_install_root_stays_writable_after_a_failed_read() {
    let tool = Path::new("/usr/local/src/repo/bin/tool");
    let read = FindingSeed {
        hooks: FindingHooks {
            registered_projects: |_| Some(vec![PathBuf::from("/usr/local/src/repo")]),
            ..STUB_HOOKS
        },
        ..stub_seed()
    };
    let failed = FindingSeed {
        hooks: FindingHooks {
            registered_projects: |_| None,
            ..STUB_HOOKS
        },
        ..stub_seed()
    };
    let named = tempfile::tempdir().unwrap();
    let root = named.path().canonicalize().unwrap();
    assert!(Places::at_roots(Some(&root), None, read).agent_writable(tool));
    assert!(Places::at_roots(Some(&root), None, failed.clone()).agent_writable(tool));
    let unread = tempfile::tempdir().unwrap();
    let other = unread.path().canonicalize().unwrap();
    assert!(!Places::at_roots(Some(&other), None, failed).agent_writable(tool));
}
