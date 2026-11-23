use super::{FindingHooks, FindingSeed, Places};
use std::path::PathBuf;
#[cfg(unix)]
use std::{ffi::OsStr, path::Path};

/// Hooks that answer nothing, for the kernel's own rows: no links, shims, prefixes,
/// projects, twins, agent temp folders or gate root; a path is
/// its one spelling and canonical as std reads it. No row whose answer needs an engine
/// check runs on these; those rows run in crazyagent on its real hooks.
pub(crate) const STUB_HOOKS: FindingHooks = FindingHooks {
    link_hops: |_| Some(Vec::new()),
    home_places: |_| Vec::new(),
    in_crazyagent_folder: |_| false,
    is_shim: |_, _| false,
    npm_prefix: |_| None,
    registered_projects: |_| Some(Vec::new()),
    projects_marker: ".crazyagent",
    data_twin: |_| None,
    one_spelling: |path| path,
    canonical: |path| std::fs::canonicalize(path),
    in_agent_tmp: |_| false,
    harness_prefix: |root| root.join(".stub-harness-prefix"),
    gate_root: || None,
};

/// The seed the kernel's rows build places on: [`STUB_HOOKS`], no layout data folder and no
/// harness install folders. Output: the seed.
pub(crate) fn stub_seed() -> FindingSeed {
    FindingSeed {
        hooks: STUB_HOOKS,
        layout_root: None,
        harness_locations: Vec::new(),
    }
}

/// Places from explicit folders and HOME on the stub seed ([`Places::new`]). Inputs: the
/// folders and HOME. Output: the places.
pub(crate) fn stub_new(folders: Vec<PathBuf>, home: Option<PathBuf>) -> Places {
    Places::new(folders, home, stub_seed())
}

/// The engine's places on the stub seed ([`Places::engine`]). Inputs: the engine root, the
/// app's folder, HOME and the PATH. Output: the places.
#[cfg(unix)]
pub(crate) fn stub_engine(
    engine_root: Option<&Path>,
    app_folder: Option<&Path>,
    home: Option<PathBuf>,
    path: Option<&OsStr>,
) -> Places {
    Places::engine(engine_root, app_folder, home, path, stub_seed())
}

/// The value of a file operation a row's setup made, or a panic naming the path it touched.
/// Inputs: the operation's result and its path. Output: the value.
#[cfg(test)]
pub(crate) fn or_named<T>(done: std::io::Result<T>, path: &std::path::Path) -> T {
    done.unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}
