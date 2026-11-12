pub mod probe;
#[cfg(all(test, unix))]
mod tests;

use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const PLACES_MAX: usize = 160;
const NAME_BYTES_MAX: usize = 64;
const PATH_BYTES_MAX: usize = 1024;
const SCRIPT_HEAD_BYTES_MAX: usize = 256;
const SHEBANG_WORDS_MAX: usize = 8;
const INTERPRETER_DEPTH_MAX: usize = 2;
const VERSION_ENTRIES_MAX: usize = 64;
const VERSIONS_PER_MANAGER_MAX: usize = 4;
const ANCESTORS_MAX: usize = 128;
const TEMP_FOLDERS: [&str; 7] = [
    "/tmp",
    "/private/tmp",
    "/var/tmp",
    "/private/var/tmp",
    "/var/folders",
    "/private/var/folders",
    "/dev/shm",
];
const HOMEBREW_FOLDERS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];
const SYSTEM_FOLDERS: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
/// Node version managers' real install folders under HOME, as (versions folder, bin folder
/// inside one version): the files their shims start, never the shims themselves.
const NODE_VERSION_FOLDERS: [(&str, &str); 5] = [
    (".volta/tools/image/node", "bin"),
    (".asdf/installs/nodejs", "bin"),
    (".local/share/mise/installs/node", "bin"),
    (".local/share/fnm/node-versions", "installation/bin"),
    (
        "Library/Application Support/fnm/node-versions",
        "installation/bin",
    ),
];

/// The owner line when tmux is found nowhere the resolver looks, with how to install it.
/// The live master's problem line, its team note and the setup row show it word for word.
#[cfg(target_os = "macos")]
pub const TMUX_MISSING: &str = "tmux is missing, so the live master's terminal can't open. Install tmux: run setup again from Settings (it needs Homebrew, from brew.sh), or run brew install tmux in Terminal. Then try again.";
/// The owner line when tmux is found nowhere the resolver looks, with how to install it.
/// The live master's problem line, its team note and the setup row show it word for word.
#[cfg(all(unix, not(target_os = "macos")))]
pub const TMUX_MISSING: &str = "tmux is missing, so the live master's terminal can't open. Install tmux: run setup again from Settings, or install it with your package manager. Then try again.";
/// The owner line where no terminal program can run, so the live master's terminal can't
/// open on this computer.
#[cfg(not(unix))]
pub const TMUX_MISSING: &str =
    "The live master's terminal can't open on this computer. Try again in a moment.";

/// The folders the resolver looks in, in order, built once from what the engine knows: no
/// program is run, no shell file is read and every folder is absolute and bounded. No
/// folder agents can write is kept ([`Places::agent_writable`]); the engine's data folder,
/// which holds the agents' HOMEs and worktrees, is kept here so its files are refused too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Places {
    folders: Vec<PathBuf>,
    home: Option<PathBuf>,
    engine_roots: Vec<PathBuf>,
}

impl Places {
    /// Builds places from explicit folders and HOME, for a caller that names its own search
    /// space. Relative, oversized and repeated folders are dropped, and so is every folder
    /// inside a project or a `.crazyagent` folder. Inputs: the folders in order and HOME.
    /// Output: the bounded places.
    pub fn new(folders: Vec<PathBuf>, home: Option<PathBuf>) -> Self {
        Self::kept(folders, home, Vec::new())
    }

    /// Keeps the folders in order, dropping relative, oversized and repeated ones and every
    /// one agents can write, at most [`PLACES_MAX`]. Inputs: the folders, HOME and the
    /// engine data folders. Output: the bounded places.
    fn kept(folders: Vec<PathBuf>, home: Option<PathBuf>, engine_roots: Vec<PathBuf>) -> Self {
        let mut places = Self {
            folders: Vec::new(),
            home: home.filter(|home| usable(home)),
            engine_roots: engine_roots
                .into_iter()
                .filter(|root| usable(root))
                .collect(),
        };
        for folder in folders {
            if places.folders.len() == PLACES_MAX {
                break;
            }
            if usable(&folder)
                && !places.folders.contains(&folder)
                && !places.agent_writable(&folder)
            {
                places.folders.push(folder);
            }
        }
        assert!(places.folders.len() <= PLACES_MAX);
        assert!(places.engine_roots.iter().all(|root| root.is_absolute()));
        places
    }

    /// Builds the engine's places: the app's own folder, the engine's setup folder, Homebrew
    /// on Apple silicon and Intel, all fixed and ahead of the PATH the engine inherited, then
    /// the PATH folders other than the system ones and those in a temp folder, the HOME
    /// folders the harness catalog uses (nvm, fnm, pnpm, ~/.local/bin, ~/.cargo/bin and
    /// more), the real installs of volta, asdf, mise and fnm, each harness's own install
    /// folders under HOME, then the system folders last. A folder agents can write is never
    /// kept ([`Places::agent_writable`]). Inputs: the engine root, the app's folder, HOME and
    /// the PATH value. Output: the bounded places.
    pub fn engine(
        engine_root: Option<&Path>,
        app_folder: Option<&Path>,
        home: Option<PathBuf>,
        path: Option<&OsStr>,
    ) -> Self {
        let system: Vec<PathBuf> = SYSTEM_FOLDERS.iter().map(PathBuf::from).collect();
        let homebrew: Vec<PathBuf> = HOMEBREW_FOLDERS.iter().map(PathBuf::from).collect();
        let mut folders: Vec<PathBuf> = Vec::new();
        folders.extend(app_folder.map(Path::to_path_buf));
        folders.extend(engine_root.map(|root| root.join("setup").join("bin")));
        folders.extend(homebrew_folders());
        if let Some(path) = path {
            folders.extend(
                std::env::split_paths(path)
                    .filter(|f| !system.contains(f) && !homebrew.contains(f) && !in_temp(f)),
            );
        }
        if let Some(home) = home.as_deref().filter(|home| home.is_absolute()) {
            folders.extend(crate::harnesses::home_places(home));
            folders.extend(node_version_folders(home));
            folders.extend(
                crate::harnesses::specs()
                    .iter()
                    .flat_map(|spec| spec.locations.iter())
                    .map(|relative| below(home, relative)),
            );
        }
        folders.extend(system);
        Self::kept(folders, home, engine_roots(engine_root))
    }

    /// Builds the engine's places from this process's HOME and PATH and the folder of the
    /// running executable. With no engine root given, this process's data folder is still
    /// refused, though its setup folder is not looked in. Input: the engine root. Output:
    /// the places ([`Places::engine`]).
    pub fn from_environment(engine_root: Option<&Path>) -> Self {
        let app = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.canonicalize().ok())
            .and_then(|exe| exe.parent().map(Path::to_path_buf));
        let data_root = engine_root.map(Path::to_path_buf).or_else(|| {
            crate::paths::Layout::current()
                .ok()
                .map(|layout| layout.root().to_path_buf())
        });
        Self {
            engine_roots: engine_roots(data_root.as_deref()),
            ..Self::engine(
                engine_root,
                app.as_deref(),
                std::env::var_os("HOME").map(PathBuf::from),
                std::env::var_os("PATH").as_deref(),
            )
        }
        .with_probed(probe::folders())
    }

    /// Adds the folders the background probes found ([`probe::folders`]), such as npm's
    /// global prefix and the login shell's PATH, after every folder known without running a
    /// program and before the system folders; folders agents can write are dropped. The
    /// probes keep only folders [`probe::kept`] passes, which also drops shared temp folders
    /// others can change. Input: the probed folders. Output: the places.
    pub fn with_probed(self, probed: Vec<PathBuf>) -> Self {
        let system: Vec<PathBuf> = SYSTEM_FOLDERS.iter().map(PathBuf::from).collect();
        let (last, mut folders): (Vec<PathBuf>, Vec<PathBuf>) = self
            .folders
            .into_iter()
            .partition(|folder| system.contains(folder));
        folders.extend(probed.into_iter().filter(|folder| !system.contains(folder)));
        folders.extend(last);
        Self::kept(folders, self.home, self.engine_roots)
    }

    /// The folders looked in, in order. Output: the folders.
    pub fn folders(&self) -> &[PathBuf] {
        &self.folders
    }

    /// HOME, for the shim checks. Output: HOME, or none.
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }

    /// Whether agents can write a folder or file, read as written and with its links
    /// resolved: inside a `.crazyagent` folder, inside a project ([`in_project`]) or inside
    /// the engine's data folder outside its `setup` folder. Input: the path. Output: the
    /// answer.
    fn agent_writable(&self, path: &Path) -> bool {
        let real = path.canonicalize().ok();
        std::iter::once(path).chain(real.as_deref()).any(|path| {
            crate::harnesses::in_crazyagent_folder(path)
                || in_project(self.home(), path)
                || in_engine_data(&self.engine_roots, path)
        })
    }
}

/// Whether a path lies in the engine's data folder outside its `setup` folder, where the
/// agents' HOMEs, worktrees and team state live, which agents write. The one rule for the
/// resolver and the harness pins. Inputs: the data folder as given and real
/// ([`engine_roots`]) and the path. Output: the answer.
pub(crate) fn in_engine_data(roots: &[PathBuf], path: &Path) -> bool {
    roots
        .iter()
        .any(|root| path.starts_with(root) && !path.starts_with(root.join("setup")))
}

/// The engine data folder as given and with its links resolved. Input: the folder, or
/// none. Output: the folders.
pub(crate) fn engine_roots(root: Option<&Path>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = root.map(Path::to_path_buf).into_iter().collect();
    if let Some(real) = root.and_then(|root| root.canonicalize().ok())
        && !roots.contains(&real)
    {
        roots.push(real);
    }
    assert!(roots.len() <= 2);
    roots
}

/// Whether a path is inside a CrazyAgent project: it or a folder above it, other than HOME
/// and `/`, holds a `.crazyagent` entry, which the sandbox never lets an agent remove. A
/// path deeper than [`ANCESTORS_MAX`] counts as inside. Inputs: HOME and the path.
/// Output: the answer.
fn in_project(home: Option<&Path>, path: &Path) -> bool {
    if path.components().count() > ANCESTORS_MAX {
        return true;
    }
    let real_home = home.and_then(|home| home.canonicalize().ok());
    path.ancestors()
        .filter(|folder| {
            folder.parent().is_some()
                && Some(*folder) != home
                && Some(*folder) != real_home.as_deref()
        })
        .any(|folder| {
            std::fs::symlink_metadata(folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY))
                .is_ok()
        })
}

/// Whether a folder is in a temp folder, which other users, or agents with the temp folder
/// grant, can write: this user's temp folder or a shared one, as written and with its links
/// resolved. Input: the folder. Output: the answer.
fn in_temp(folder: &Path) -> bool {
    let owner = std::env::temp_dir();
    let real = folder.canonicalize().ok();
    std::iter::once(folder)
        .chain(real.as_deref())
        .any(|folder| {
            (owner.is_absolute() && folder.starts_with(&owner))
                || TEMP_FOLDERS.iter().any(|temp| folder.starts_with(temp))
        })
}

/// How a resolved program runs: as the program itself, or as a script its interpreter reads,
/// named by path in its first line or by name through `env`, which needs a search path.
/// `by_name` also marks an interpreter found past a version-manager shim the first line
/// names, so the launch names the real install and never the shim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Native,
    Script {
        interpreter: Box<Resolved>,
        arguments: Vec<String>,
        by_name: bool,
    },
}

/// One tool the resolver found: the entry as found, the real file it names and how it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub entry: PathBuf,
    pub program: PathBuf,
    pub kind: Kind,
}

impl Resolved {
    /// The words that start this tool with no search path: the real file, which the system
    /// runs itself for a program or a script whose first line names a program by path;
    /// otherwise the interpreter's own words, the first line's arguments, then the script's
    /// real file, for a script that asks `env` by name, names a shim, or names an interpreter
    /// that is itself a script, which macOS does not run. Output: the absolute words.
    pub fn argv(&self) -> Vec<OsString> {
        let words: Vec<OsString> = match &self.kind {
            Kind::Native => vec![self.program.clone().into_os_string()],
            Kind::Script {
                interpreter,
                by_name: false,
                ..
            } if interpreter.kind == Kind::Native => {
                vec![self.program.clone().into_os_string()]
            }
            Kind::Script {
                interpreter,
                arguments,
                ..
            } => {
                let mut words = interpreter.argv();
                words.extend(arguments.iter().map(OsString::from));
                words.push(self.program.clone().into_os_string());
                words
            }
        };
        assert!(Path::new(&words[0]).is_absolute());
        words
    }

    /// Every real file this tool runs or reads to start, the program first and then each
    /// interpreter, which is exactly what a sandbox grants it. A Node package's own folder is
    /// not among them; the sandbox adds it from the program's npm prefix. Output: the files.
    pub fn grants(&self) -> Vec<PathBuf> {
        let mut files = vec![self.program.clone()];
        if let Kind::Script { interpreter, .. } = &self.kind {
            files.extend(interpreter.grants());
        }
        assert!(files.iter().all(|file| file.is_absolute()));
        files
    }

    /// The interpreter a script needs, none for a native program. Output: the interpreter.
    pub fn interpreter(&self) -> Option<&Resolved> {
        match &self.kind {
            Kind::Native => None,
            Kind::Script { interpreter, .. } => Some(interpreter),
        }
    }
}

/// Why a tool cannot run, each with its stable code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    NotFound,
    NeedsInterpreter,
    WrongVersion,
}

impl Reason {
    /// The stable code clients and records match on. Output: the code.
    pub fn code(self) -> &'static str {
        match self {
            Self::NotFound => "tool_not_found",
            Self::NeedsInterpreter => "tool_needs_interpreter",
            Self::WrongVersion => "tool_wrong_version",
        }
    }
}

/// A tool that cannot run: which one, why, the interpreter it needs when that is what is
/// missing, and the folders looked in, kept apart from the owner line so the line is fixed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    pub tool: String,
    pub reason: Reason,
    pub needs: Option<String>,
    pub looked: Vec<PathBuf>,
}

impl Missing {
    /// The one owner line for this tool and reason, with how to install what is missing;
    /// the same tool and reason always give the same line. Output: the line.
    pub fn line(&self) -> String {
        match (self.reason, self.tool.as_str(), self.needs.as_deref()) {
            (Reason::NotFound, "tmux", _) => TMUX_MISSING.to_owned(),
            (Reason::NotFound, tool, _) => {
                format!("{tool} is missing. {} Then try again.", install_hint(tool))
            }
            (Reason::NeedsInterpreter, tool, needs) => {
                let needs = needs.unwrap_or("a program to run it");
                format!(
                    "{tool} needs {needs}, which is missing. {} Then try again.",
                    install_hint(needs)
                )
            }
            (Reason::WrongVersion, tool, _) => {
                format!("{tool} is too old for CrazyAgent. Update it, then try again.")
            }
        }
    }
}

/// Finds a tool by its program name wherever the places look, passing over version-manager
/// shims, files in a `.crazyagent` folder and files others can change, then reads how it
/// runs; a miss asks the background probes to look again ([`probe::missed`]). Harness
/// launches keep their pinned entry and call [`resolve_entry`] instead.
/// Inputs: the program name and the places. Output: the tool, or why it cannot run.
pub fn resolve(name: &str, places: &Places) -> Result<Resolved, Missing> {
    resolve_at(name, places, 0)
}

/// Reads how an entry already chosen runs, such as a harness file the engine pinned: its
/// real file and, for a script, the interpreter its first line names, found by
/// [`resolve`]. The entry and every interpreter pass the same checks as a tool found by
/// name. Inputs: the absolute entry and the places. Output: the tool, or why it cannot run.
pub fn resolve_entry(entry: &Path, places: &Places) -> Result<Resolved, Missing> {
    entry_at(entry, places, 0)
}

fn resolve_at(name: &str, places: &Places, depth: usize) -> Result<Resolved, Missing> {
    assert!(depth <= INTERPRETER_DEPTH_MAX);
    let not_found = || Missing {
        tool: name.to_owned(),
        reason: Reason::NotFound,
        needs: None,
        looked: places.folders.clone(),
    };
    if !valid_name(name) {
        return Err(not_found());
    }
    let Some(found) = places
        .folders
        .iter()
        .map(|folder| folder.join(name))
        .find(|candidate| usable_candidate(places, candidate))
    else {
        probe::missed();
        return Err(not_found());
    };
    entry_at(&found, places, depth).map_err(|missing| Missing {
        tool: name.to_owned(),
        ..missing
    })
}

fn entry_at(entry: &Path, places: &Places, depth: usize) -> Result<Resolved, Missing> {
    let tool = entry
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("the program")
        .to_owned();
    let program = entry
        .canonicalize()
        .ok()
        .filter(|_| entry.is_absolute() && usable_candidate(places, entry))
        .ok_or_else(|| Missing {
            tool: tool.clone(),
            reason: Reason::NotFound,
            needs: None,
            looked: entry.parent().map(Path::to_path_buf).into_iter().collect(),
        })?;
    let Some(shebang) = shebang(&program) else {
        return Ok(Resolved {
            entry: entry.to_path_buf(),
            program,
            kind: Kind::Native,
        });
    };
    let needs = |name: &str| Missing {
        tool: tool.clone(),
        reason: Reason::NeedsInterpreter,
        needs: Some(name.to_owned()),
        looked: places.folders.clone(),
    };
    let interpreter_name = shebang.interpreter_name();
    if depth == INTERPRETER_DEPTH_MAX {
        return Err(needs(&interpreter_name));
    }
    let through_shim =
        shebang.by_name.is_none() && crate::harnesses::is_shim(places.home(), &shebang.interpreter);
    let interpreter = match &shebang.by_name {
        Some(name) => named_interpreter(name, &program, places, depth + 1),
        None if through_shim => resolve_at(&interpreter_name, places, depth + 1),
        None => entry_at(&shebang.interpreter, places, depth + 1),
    }
    .map_err(|_| needs(&interpreter_name))?;
    Ok(Resolved {
        entry: entry.to_path_buf(),
        program,
        kind: Kind::Script {
            interpreter: Box::new(interpreter),
            arguments: shebang.arguments,
            by_name: shebang.by_name.is_some() || through_shim,
        },
    })
}

/// Finds the interpreter a script's first line asks `env` for by name: for `node`, first the
/// one in the script's own npm prefix ([`crate::harnesses::node_prefix`]), so a harness
/// installed under one Node starts under that Node, then the places in order. Inputs: the
/// name, the script's real file, the places and the depth. Output: the interpreter, or why.
fn named_interpreter(
    name: &str,
    script: &Path,
    places: &Places,
    depth: usize,
) -> Result<Resolved, Missing> {
    assert!(depth <= INTERPRETER_DEPTH_MAX);
    let own = (name == "node")
        .then(|| crate::harnesses::node_prefix(script))
        .flatten()
        .and_then(|prefix| entry_at(&prefix.join("bin").join(name), places, depth).ok());
    match own {
        Some(node) => Ok(node),
        None => resolve_at(name, places, depth),
    }
}

/// A script's first line: the absolute interpreter, or the program name `env` looks up,
/// and the arguments before the script.
struct Shebang {
    interpreter: PathBuf,
    by_name: Option<String>,
    arguments: Vec<String>,
}

impl Shebang {
    fn interpreter_name(&self) -> String {
        match &self.by_name {
            Some(name) => name.clone(),
            None => self
                .interpreter
                .file_name()
                .and_then(OsStr::to_str)
                .unwrap_or("its interpreter")
                .to_owned(),
        }
    }
}

/// Reads the `#!` line of a file, at most [`SCRIPT_HEAD_BYTES_MAX`] bytes: an absolute
/// interpreter with its words, or `env` with the program it names (its flags and
/// assignments skipped). Input: the real file. Output: the line, or none for a program
/// that is not a script or a line that names no usable interpreter.
fn shebang(file: &Path) -> Option<Shebang> {
    use std::io::Read;
    let mut head = Vec::with_capacity(SCRIPT_HEAD_BYTES_MAX);
    std::fs::File::open(file)
        .ok()?
        .take(SCRIPT_HEAD_BYTES_MAX as u64)
        .read_to_end(&mut head)
        .ok()?;
    let line = head.strip_prefix(b"#!")?;
    let line = &line[..line.iter().position(|b| *b == b'\n').unwrap_or(line.len())];
    let line = std::str::from_utf8(line).ok()?.trim();
    let words: Vec<&str> = line.split_whitespace().take(SHEBANG_WORDS_MAX).collect();
    let interpreter = PathBuf::from(words.first()?);
    if !usable(&interpreter) {
        return None;
    }
    let rest = &words[1..];
    if interpreter.file_name() != Some(OsStr::new("env")) {
        return Some(Shebang {
            interpreter,
            by_name: None,
            arguments: rest.iter().map(|word| (*word).to_owned()).collect(),
        });
    }
    let start = rest
        .iter()
        .position(|word| !word.starts_with('-') && !word.contains('='))?;
    let program = rest[start];
    let arguments: Vec<String> = rest[start + 1..].iter().map(|w| (*w).to_owned()).collect();
    if program.starts_with('/') {
        return Some(Shebang {
            interpreter: PathBuf::from(program),
            by_name: None,
            arguments,
        });
    }
    valid_name(program).then(|| Shebang {
        interpreter,
        by_name: Some(program.to_owned()),
        arguments,
    })
}

/// Homebrew's program folders on Apple silicon and Intel. Only a build with the
/// `clean-mac-gate` feature, never a release, reads them under the gate's root
/// ([`gate_root`]), so the clean-Mac gate sees none of this computer's Homebrew tools.
/// Output: the two folders.
fn homebrew_folders() -> Vec<PathBuf> {
    let root = gate_root();
    HOMEBREW_FOLDERS
        .iter()
        .map(|folder| match &root {
            Some(root) => below(root, folder.trim_start_matches('/')),
            None => PathBuf::from(folder),
        })
        .collect()
}

/// The clean-Mac gate's stand-in system root, from `CRAZYAGENT_GATE_SYSTEM_ROOT` when it
/// names an absolute folder. Output: the root, or none.
#[cfg(feature = "clean-mac-gate")]
fn gate_root() -> Option<PathBuf> {
    std::env::var_os("CRAZYAGENT_GATE_SYSTEM_ROOT")
        .map(PathBuf::from)
        .filter(|root| usable(root))
}

/// A build without the clean-Mac gate feature, every release, has no stand-in system root.
/// Output: none.
#[cfg(not(feature = "clean-mac-gate"))]
fn gate_root() -> Option<PathBuf> {
    None
}

/// The `bin` folders of the newest Node versions each version manager installed, at most
/// [`VERSIONS_PER_MANAGER_MAX`] each from at most [`VERSION_ENTRIES_MAX`] entries read.
/// Input: an absolute HOME. Output: the folders, newest first per manager.
fn node_version_folders(home: &Path) -> Vec<PathBuf> {
    assert!(home.is_absolute());
    let mut folders = Vec::new();
    for (versions, bin) in NODE_VERSION_FOLDERS {
        let versions = below(home, versions);
        let Ok(entries) = std::fs::read_dir(&versions) else {
            continue;
        };
        let mut found: Vec<((u64, u64, u64), String)> = entries
            .take(VERSION_ENTRIES_MAX)
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                Some((version(&name)?, name))
            })
            .collect();
        found.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        found.truncate(VERSIONS_PER_MANAGER_MAX);
        folders.extend(
            found
                .into_iter()
                .map(|(_, name)| below(&versions.join(name), bin)),
        );
    }
    assert!(folders.len() <= NODE_VERSION_FOLDERS.len() * VERSIONS_PER_MANAGER_MAX);
    folders
}

/// Reads a version folder name such as `v22.11.0` or `22.11.0`. Input: the name. Output:
/// the major, minor and patch numbers, or none for any other name.
fn version(name: &str) -> Option<(u64, u64, u64)> {
    let bare = name.strip_prefix('v').unwrap_or(name);
    let mut parts = bare.splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts
        .next()?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((major, minor, patch))
}

/// The owner's next step to install a tool by name. Input: the program name. Output: the
/// sentence.
fn install_hint(tool: &str) -> &'static str {
    match tool {
        "node" | "npm" => "Install Node.js from nodejs.org, or run brew install node in Terminal.",
        "codex" => "Install Codex: run npm install -g @openai/codex in Terminal.",
        "claude" => {
            "Install Claude Code: run npm install -g @anthropic-ai/claude-code in Terminal."
        }
        "git" => "Install git: run xcode-select --install in Terminal.",
        _ => "Install it, or run setup again from Settings.",
    }
}

/// Whether a found file can be used: an executable file that is not a version-manager shim,
/// that agents cannot write, as found or through its links ([`Places::agent_writable`]),
/// and that no other user can change ([`crate::service::changeable_past_sticky`]). Inputs: the places and the file. Output: the answer.
fn usable_candidate(places: &Places, candidate: &Path) -> bool {
    executable_file(candidate)
        && candidate.canonicalize().is_ok()
        && !crate::harnesses::is_shim(places.home(), candidate)
        && !places.agent_writable(candidate)
        && crate::service::changeable_past_sticky(candidate).is_none()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES_MAX
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
}

/// Whether a path may name a place: absolute, bounded, not `/` and plain, with no `.` or
/// `..` part that would read one folder as another. Input: the path. Output: the answer.
fn usable(path: &Path) -> bool {
    use std::path::Component;
    path.is_absolute()
        && path.as_os_str().len() <= PATH_BYTES_MAX
        && path.as_os_str() != OsStr::new("/")
        && path.components().all(|part| {
            matches!(
                part,
                Component::Prefix(_) | Component::RootDir | Component::Normal(_)
            )
        })
}

fn below(base: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(base.to_path_buf(), |at, part| at.join(part))
}

fn executable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && runnable(&metadata))
}

/// Reports whether file metadata grants any execute permission.
/// Takes the metadata. Returns true when any of the three execute bits is set.
#[cfg(unix)]
fn runnable(metadata: &std::fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

/// Reports that a file may be run where the platform has no execute bit.
/// Takes the metadata, unused. Returns true.
#[cfg(not(unix))]
fn runnable(_metadata: &std::fs::Metadata) -> bool {
    true
}
