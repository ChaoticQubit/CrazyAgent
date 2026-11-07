use super::{major_of, program_for, tool_for};

const CARGO: &[&str] = &["cargo", "update", "--workspace", "--offline"];
const UV: &[&str] = &["uv", "lock"];
const NPM: &[&str] = &["npm", "install", "--package-lock-only", "--ignore-scripts"];
const PNPM: &[&str] = &["pnpm", "install", "--lockfile-only", "--ignore-scripts"];
const YARN: &[&str] = &["yarn", "install", "--mode", "update-lockfile"];
const POETRY: &[&str] = &["poetry", "lock"];
const POETRY_1: &[&str] = &["poetry", "lock", "--no-update"];

/// Every row of the plan's table: each known lockfile names its tool's lockfile-only
/// command at any version, Yarn 2 or later and Poetry 2 or later by their own commands,
/// Yarn 1 none and Poetry 1 `--no-update`; a missing tool, another file or a path that
/// leaves the repository names none.
#[test]
fn every_known_lockfile_names_its_tool_command_by_the_tool_major_version() {
    let rows: [(&str, Option<u32>, Option<&[&str]>); 21] = [
        ("Cargo.lock", Some(1), Some(CARGO)),
        ("crates/web/Cargo.lock", Some(1), Some(CARGO)),
        ("uv.lock", Some(0), Some(UV)),
        ("uv.lock", Some(1), Some(UV)),
        ("package-lock.json", Some(10), Some(NPM)),
        ("web/package-lock.json", Some(7), Some(NPM)),
        ("pnpm-lock.yaml", Some(9), Some(PNPM)),
        ("yarn.lock", Some(2), Some(YARN)),
        ("yarn.lock", Some(4), Some(YARN)),
        ("yarn.lock", Some(1), None),
        ("yarn.lock", Some(0), None),
        ("poetry.lock", Some(2), Some(POETRY)),
        ("poetry.lock", Some(3), Some(POETRY)),
        ("poetry.lock", Some(1), Some(POETRY_1)),
        ("Cargo.lock", None, None),
        ("poetry.lock", None, None),
        ("Gemfile.lock", Some(2), None),
        ("cargo.lock", Some(1), None),
        ("Cargo.lock.orig", Some(1), None),
        ("../Cargo.lock", Some(1), None),
        ("/Cargo.lock", Some(1), None),
    ];
    for (path, major, command) in rows {
        assert_eq!(tool_for(path, major), command, "{path} at {major:?}");
    }
}

/// The program whose version decides a lockfile's command is the one the command runs.
#[test]
fn each_known_lockfile_names_the_program_its_command_runs() {
    for (path, program) in [
        ("Cargo.lock", Some("cargo")),
        ("a/b/uv.lock", Some("uv")),
        ("package-lock.json", Some("npm")),
        ("pnpm-lock.yaml", Some("pnpm")),
        ("yarn.lock", Some("yarn")),
        ("poetry.lock", Some("poetry")),
        ("Gemfile.lock", None),
        ("", None),
        ("a/../Cargo.lock", None),
        ("./Cargo.lock", None),
    ] {
        assert_eq!(program_for(path), program, "{path}");
        if let Some(program) = program {
            assert_eq!(
                tool_for(path, Some(9)).map(|command| command[0]),
                Some(program)
            );
        }
    }
}

/// A tool's major version is the leading number of its printed version, whatever words
/// come before it; none when it printed no version or a number past `u32`.
#[test]
fn a_tool_major_version_is_read_from_what_its_version_flag_printed() {
    for (printed, major) in [
        ("cargo 1.98.1 (48a229cea 2026-09-01)\n", Some(1)),
        ("uv 0.5.31 (Homebrew 2025-02-11)\n", Some(0)),
        ("10.9.2\n", Some(10)),
        ("v22.11.0\n", Some(22)),
        ("Poetry (version 2.1.1)\n", Some(2)),
        ("Poetry (version 1.8.5)\n", Some(1)),
        ("1.22.22\n", Some(1)),
        ("", None),
        ("env: yarn: No such file or directory\n", None),
        ("yarn 12\n", None),
        ("yarn 99999999999.0\n", None),
    ] {
        assert_eq!(major_of(printed.as_bytes()), major, "{printed:?}");
    }
}
