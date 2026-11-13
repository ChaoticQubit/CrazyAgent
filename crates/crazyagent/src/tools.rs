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
/// Linux package folders often missing from the engine's PATH: Linuxbrew, where setup
/// installs tmux, and the NixOS system and default profiles. They are looked in before the
/// system folders, under the same entry checks as every other place.
#[cfg(target_os = "linux")]
const LINUX_FOLDERS: [&str; 3] = [
    "/home/linuxbrew/.linuxbrew/bin",
    "/run/current-system/sw/bin",
    "/nix/var/nix/profiles/default/bin",
];
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

/// The data root of the engine this process runs, recorded once at its start
/// ([`record_engine_root`]).
static ENGINE_ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Records the data root of the engine this process runs, once, at its start, so a launch
/// looks in the same places setup does ([`Places::from_environment`]). Only the engine
/// binary records it; recording another root later is a bug. Input: the absolute root.
pub fn record_engine_root(root: &Path) {
    assert!(root.is_absolute());
    let kept = ENGINE_ROOT.get_or_init(|| root.to_path_buf());
    assert_eq!(kept, root, "one engine root per process");
}

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
/// folder agents can write is kept ([`Places::agent_writable`]), the engine's data folder
/// included; one in the engine's `setup` folder is set aside so [`resolve`] names a tool
/// there, and the engine's own places set aside a PATH or probed temp folder the same way
/// ([`Places::set_aside_temp`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Places {
    folders: Vec<PathBuf>,
    home: Option<PathBuf>,
    engine_roots: Vec<PathBuf>,
    temp_refused: bool,
    temps: Vec<PathBuf>,
    refused_setup: Vec<PathBuf>,
    refused_temp: Vec<PathBuf>,
}

impl Places {
    /// Builds places from explicit folders and HOME, for a caller that names its own search
    /// space. Relative, oversized and repeated folders are dropped, and so is every folder
    /// inside a project or a `.crazyagent` folder. Inputs: the folders in order and HOME.
    /// Output: the bounded places.
    pub fn new(folders: Vec<PathBuf>, home: Option<PathBuf>) -> Self {
        Self::kept(folders, home, Vec::new(), false)
    }

    /// Keeps the folders in order, dropping relative, oversized and repeated ones and every
    /// one agents can write, at most [`PLACES_MAX`], and sets aside one of those in an engine
    /// `setup` folder ([`Places::in_engine_setup`]). Inputs: the folders, HOME, the engine
    /// data folders and whether probed temp folders are refused. Output: the bounded places.
    fn kept(
        folders: Vec<PathBuf>,
        home: Option<PathBuf>,
        engine_roots: Vec<PathBuf>,
        temp_refused: bool,
    ) -> Self {
        let mut places = Self {
            folders: Vec::new(),
            home: home.filter(|home| usable(home)),
            engine_roots: engine_roots
                .into_iter()
                .filter(|root| usable(root))
                .collect(),
            temp_refused,
            temps: if temp_refused {
                temp_roots()
            } else {
                Vec::new()
            },
            refused_setup: Vec::new(),
            refused_temp: Vec::new(),
        };
        for folder in folders {
            if places.folders.len() == PLACES_MAX {
                break;
            }
            if !usable(&folder) || places.folders.contains(&folder) {
                continue;
            }
            if !places.agent_writable(&folder) {
                places.folders.push(folder);
            } else if places.in_engine_setup(&folder)
                && !places.refused_setup.contains(&folder)
                && places.refused_setup.len() < PLACES_MAX
            {
                places.refused_setup.push(folder);
            }
        }
        assert!(places.folders.len() <= PLACES_MAX);
        assert!(places.refused_setup.len() <= PLACES_MAX);
        assert!(places.engine_roots.iter().all(|root| root.is_absolute()));
        places
    }

    /// Replaces the temp folders the engine's places refuse, so a row can name a folder
    /// outside every temp folder while each folder it makes lies in the real one. Input: the
    /// temp folders. Output: the places.
    #[cfg(test)]
    fn with_temps(mut self, temps: Vec<PathBuf>) -> Self {
        assert!(self.temp_refused);
        self.temps = temps;
        self
    }

    /// Sets aside temp folders the engine's places refuse, never looked in, so [`resolve`]
    /// names a tool found only there instead of calling it missing; at most [`PLACES_MAX`].
    /// Input: the folders, absolute ones kept. Output: the places.
    fn set_aside_temp(mut self, folders: impl IntoIterator<Item = PathBuf>) -> Self {
        for folder in folders {
            if self.refused_temp.len() == PLACES_MAX {
                break;
            }
            if usable(&folder) && !self.refused_temp.contains(&folder) {
                self.refused_temp.push(folder);
            }
        }
        assert!(self.refused_temp.len() <= PLACES_MAX);
        assert!(self.temp_refused || self.refused_temp.is_empty());
        self
    }

    /// Whether a folder lies in an engine data folder's `setup` folder, which agents can
    /// write only when that data folder sits inside a project or a `.crazyagent` folder.
    /// Input: the folder. Output: the answer.
    fn in_engine_setup(&self, folder: &Path) -> bool {
        self.engine_roots
            .iter()
            .any(|root| folder.starts_with(root.join("setup")))
    }

    /// Builds the engine's places: the app's own folder, Homebrew on Apple silicon and Intel
    /// ahead of the PATH the engine inherited, then the PATH folders other than the system
    /// ones, those in a temp folder set aside, the HOME folders the harness catalog uses (nvm,
    /// fnm, pnpm, ~/.local/bin, ~/.cargo/bin and more), the real installs of volta, asdf, mise
    /// and fnm, each harness's own install folders under HOME, the system folders, and last the
    /// engine's own folders ([`engine_folders`]): the owner's installs win, the app's are the
    /// fallback. A folder agents can write is never kept ([`Places::agent_writable`]).
    /// Inputs: the engine root, the app's folder, HOME and the PATH. Output: the places.
    pub fn engine(
        engine_root: Option<&Path>,
        app_folder: Option<&Path>,
        home: Option<PathBuf>,
        path: Option<&OsStr>,
    ) -> Self {
        let system: Vec<PathBuf> = SYSTEM_FOLDERS.iter().map(PathBuf::from).collect();
        let homebrew: Vec<PathBuf> = HOMEBREW_FOLDERS.iter().map(PathBuf::from).collect();
        let mut folders: Vec<PathBuf> = Vec::new();
        let mut temp: Vec<PathBuf> = Vec::new();
        folders.extend(app_folder.map(Path::to_path_buf));
        folders.extend(homebrew_folders());
        if let Some(path) = path {
            for folder in
                std::env::split_paths(path).filter(|f| !system.contains(f) && !homebrew.contains(f))
            {
                if in_temp(&folder) {
                    temp.push(folder);
                } else {
                    folders.push(folder);
                }
            }
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
        #[cfg(target_os = "linux")]
        folders.extend(LINUX_FOLDERS.iter().map(PathBuf::from));
        folders.extend(system);
        folders.extend(engine_root.into_iter().flat_map(engine_folders));
        Self::kept(folders, home, engine_roots(engine_root), true).set_aside_temp(temp)
    }

    /// Builds the engine's places from this process's HOME and PATH and the folder of the
    /// running executable. With no engine root given, a launch takes the one this process's
    /// engine recorded ([`record_engine_root`]), so it looks where setup does; with none,
    /// the data folder is still refused, though its setup folder is not looked in. Input:
    /// the engine root. Output: the places ([`Places::engine`]).
    pub fn from_environment(engine_root: Option<&Path>) -> Self {
        Self::at_roots(engine_root, ENGINE_ROOT.get().map(PathBuf::as_path))
    }

    /// Builds the engine's places as [`Places::from_environment`] does, with the engine
    /// root this process recorded given explicitly so rows never record the global. Inputs:
    /// the engine root given, if any, and the recorded one. Output: the places.
    fn at_roots(engine_root: Option<&Path>, recorded: Option<&Path>) -> Self {
        let engine_root = engine_root.or(recorded);
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
    /// program and before the system folders and the engine's own folders, which stay last;
    /// folders agents can write are dropped, and on the engine's places ([`Places::engine`])
    /// one in a temp folder, which agents with the owner's temp folder grant or other users
    /// write, is set aside ([`Places::set_aside_temp`]). Input: the folders. Output: the places.
    pub fn with_probed(self, probed: Vec<PathBuf>) -> Self {
        let mut tail: Vec<PathBuf> = SYSTEM_FOLDERS.iter().map(PathBuf::from).collect();
        tail.extend(
            self.engine_roots
                .iter()
                .flat_map(|root| engine_folders(root)),
        );
        let temp_refused = self.temp_refused;
        let (last, mut folders): (Vec<PathBuf>, Vec<PathBuf>) = self
            .folders
            .into_iter()
            .partition(|folder| tail.contains(folder));
        let (temp, probed): (Vec<PathBuf>, Vec<PathBuf>) = probed
            .into_iter()
            .filter(|folder| !tail.contains(folder))
            .partition(|folder| temp_refused && in_temp(folder));
        folders.extend(probed);
        folders.extend(last);
        let set_aside = self.refused_setup;
        let carried = self.refused_temp;
        let mut places = Self::kept(folders, self.home, self.engine_roots, temp_refused);
        for folder in set_aside {
            if !places.refused_setup.contains(&folder) && places.refused_setup.len() < PLACES_MAX {
                places.refused_setup.push(folder);
            }
        }
        places.set_aside_temp(carried.into_iter().chain(temp))
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
    /// resolved: inside a `.crazyagent` folder, inside a project ([`in_project`]), inside
    /// the engine's data folder outside its `setup` folder, or inside an agent's short temp
    /// folder ([`crate::sandbox::toolchains::in_agent_tmp`]). Input: the path. Output: the answer.
    pub(crate) fn agent_writable(&self, path: &Path) -> bool {
        let real = path.canonicalize().ok();
        std::iter::once(path).chain(real.as_deref()).any(|path| {
            crate::harnesses::in_crazyagent_folder(path)
                || in_project(self.home(), path)
                || in_engine_data(&self.engine_roots, path)
                || crate::sandbox::toolchains::in_agent_tmp(path)
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

/// The engine's own folders, looked in after every folder of the owner: its setup `bin`,
/// then its npm harness prefix's `bin` ([`crate::setup::harness_prefix`]). Input: the engine
/// root. Output: the two folders in that order.
fn engine_folders(root: &Path) -> [PathBuf; 2] {
    [
        root.join("setup").join("bin"),
        crate::setup::harness_prefix(root).join("bin"),
    ]
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

/// Whether a file found through a place outside every temp folder really lies in one, as an
/// owner link from `~/.local/bin` into the temp folder does, which agents with the owner's
/// temp folder grant can replace; a file whose place is itself in a temp folder (a test HOME,
/// an engine root, a mounted app) does not. Inputs: the file as written, its real path and
/// the temp folders. Output: the answer.
pub(crate) fn leads_into_temp(written: &Path, real: &Path, temps: &[PathBuf]) -> bool {
    let inside = |path: &Path| temps.iter().any(|temp| path.starts_with(temp));
    !inside(written) && inside(real)
}

/// The temp folders: the shared ones and this user's own, as given and with its links
/// resolved, so a path matches however it is written; an owner temp folder of `/` is
/// dropped. Output: the absolute folders.
pub(crate) fn temp_roots() -> Vec<PathBuf> {
    let owner = std::env::temp_dir();
    let real = owner.canonicalize().ok();
    let mut roots: Vec<PathBuf> = TEMP_FOLDERS.iter().map(PathBuf::from).collect();
    roots.extend(
        std::iter::once(owner)
            .chain(real)
            .filter(|root| root.is_absolute() && root.parent().is_some()),
    );
    assert!(roots.len() <= TEMP_FOLDERS.len() + 2);
    roots
}

/// Whether a folder is in a temp folder, which other users, or agents with the temp folder
/// grant, can write: this user's temp folder or a shared one, as written and with its links
/// resolved. Input: the folder. Output: the answer.
fn in_temp(folder: &Path) -> bool {
    in_temp_of(folder, &temp_roots())
}

/// Whether a folder is in one of the temp folders given, as written and with its links
/// resolved; none given refuses nothing. Inputs: the folder and the temp folders. Output:
/// the answer.
pub(crate) fn in_temp_of(folder: &Path, temps: &[PathBuf]) -> bool {
    if temps.is_empty() {
        return false;
    }
    let real = folder.canonicalize().ok();
    std::iter::once(folder)
        .chain(real.as_deref())
        .any(|folder| temps.iter().any(|temp| folder.starts_with(temp)))
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
    Refused,
}

impl Reason {
    /// The stable code clients and records match on. Output: the code.
    pub fn code(self) -> &'static str {
        match self {
            Self::NotFound => "tool_not_found",
            Self::NeedsInterpreter => "tool_needs_interpreter",
            Self::WrongVersion => "tool_wrong_version",
            Self::Refused => "tool_refused",
        }
    }
}

/// Why the resolver passed over a file it found: a part another user or group can change,
/// named so the owner knows what to fix, a file agents can write, a version-manager shim
/// with no real install found behind it, or a file in a temp folder the engine's places
/// refuse or reached through a link into one, which agents with the owner's temp folder
/// grant or other users write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    OthersCanChange(PathBuf),
    AgentsCanWrite,
    Shim,
    InTemp,
}

/// A file the resolver found and passed over, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub file: PathBuf,
    pub why: Refusal,
}

/// A tool that cannot run: which one, why, the interpreter it needs when that is what is
/// missing, the folders looked in and the first file passed over when no usable one was
/// found, boxed so the error stays small, kept apart from the owner line so it is fixed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    pub tool: String,
    pub reason: Reason,
    pub needs: Option<String>,
    pub looked: Vec<PathBuf>,
    pub refused: Option<Box<Refused>>,
}

impl Missing {
    /// The one owner line for this tool and reason, with how to install what is missing, or
    /// for a file passed over, the file, why and what to change, never an install hint; the
    /// same tool, reason and file always give the same line. Output: the line.
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
            (Reason::Refused, tool, _) => match &self.refused {
                Some(refused) => refused_line(tool, refused),
                None => format!(
                    "{tool} can't start: the file found for it is not used, since others can change it. Check who can write it, then try again."
                ),
            },
        }
    }
}

/// Finds a tool by its program name wherever the places look, passing over version-manager
/// shims, files agents can write and files others can change, then reads how it runs; a
/// miss names the first file passed over ([`Refused`]), a set-aside `setup` folder's first,
/// and asks the background probes to look again ([`probe::missed`]). Harness launches call
/// [`resolve_entry`]. Inputs: the program name and the places. Output: the tool, or why not.
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
        refused: None,
    };
    if !valid_name(name) {
        return Err(not_found());
    }
    let in_setup = places
        .refused_setup
        .iter()
        .map(|folder| (folder.join(name), false));
    let in_temp_folder = places
        .refused_temp
        .iter()
        .map(|folder| (folder.join(name), true));
    let mut refused = in_setup
        .chain(in_temp_folder)
        .find_map(|(candidate, temp)| {
            let why = match check_candidate(places, &candidate)? {
                Err(why) => why,
                Ok(()) if temp => Refusal::InTemp,
                Ok(()) => return None,
            };
            Some(Refused {
                file: candidate,
                why,
            })
        });
    let found =
        places.folders.iter().map(|folder| folder.join(name)).find(
            |candidate| match check_candidate(places, candidate) {
                Some(Ok(())) => true,
                Some(Err(why)) => {
                    refused.get_or_insert_with(|| Refused {
                        file: candidate.clone(),
                        why,
                    });
                    false
                }
                None => false,
            },
        );
    let Some(found) = found else {
        probe::missed();
        return Err(match refused {
            Some(refused) => Missing {
                reason: Reason::Refused,
                refused: Some(Box::new(refused)),
                ..not_found()
            },
            None => not_found(),
        });
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
    let not_there = || Missing {
        tool: tool.clone(),
        reason: Reason::NotFound,
        needs: None,
        looked: entry.parent().map(Path::to_path_buf).into_iter().collect(),
        refused: None,
    };
    let program = entry
        .canonicalize()
        .ok()
        .filter(|_| entry.is_absolute())
        .ok_or_else(not_there)?;
    match check_candidate(places, entry) {
        Some(Ok(())) => {}
        Some(Err(why)) => {
            return Err(Missing {
                reason: Reason::Refused,
                refused: Some(Box::new(Refused {
                    file: entry.to_path_buf(),
                    why,
                })),
                ..not_there()
            });
        }
        None => return Err(not_there()),
    }
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
        refused: None,
    };
    let interpreter_name = shebang.interpreter_name();
    if depth == INTERPRETER_DEPTH_MAX {
        return Err(needs(&interpreter_name));
    }
    let through_shim =
        shebang.by_name.is_none() && crate::harnesses::is_shim(places.home(), &shebang.interpreter);
    let into_temp = places.temp_refused
        && shebang
            .interpreter
            .canonicalize()
            .is_ok_and(|real| leads_into_temp(entry, &real, &places.temps));
    let interpreter = match &shebang.by_name {
        Some(name) => named_interpreter(name, &program, places, depth + 1),
        None if through_shim => resolve_at(&interpreter_name, places, depth + 1),
        None if into_temp => Err(Missing {
            reason: Reason::Refused,
            refused: Some(Box::new(Refused {
                file: shebang.interpreter.clone(),
                why: Refusal::InTemp,
            })),
            ..not_there()
        }),
        None => entry_at(&shebang.interpreter, places, depth + 1),
    }
    .map_err(|missing| match missing.reason {
        Reason::Refused => Missing {
            tool: tool.clone(),
            ..missing
        },
        _ => needs(&interpreter_name),
    })?;
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
pub(crate) fn homebrew_folders() -> Vec<PathBuf> {
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

/// The owner line for a file the resolver passed over: the file, why and what to change,
/// never how to install the tool, which would only answer that it is installed already.
/// Inputs: the tool and the file passed over. Output: the line.
fn refused_line(tool: &str, refused: &Refused) -> String {
    let file = refused.file.display();
    match &refused.why {
        Refusal::OthersCanChange(part) => format!(
            "{tool} can't start: {file} is not used because another user or group can change {}. Make it writable only by you or an admin (for example chmod go-w), then try again.",
            part.display()
        ),
        Refusal::AgentsCanWrite => format!(
            "{tool} can't start: {file} is not used because agents can change it there (inside a project, a .crazyagent folder, an agent's temp folder or the app's data). Use a copy outside those places, then try again."
        ),
        Refusal::Shim => format!(
            "{tool} can't start: {file} is a version manager shim with no install found behind it. Add a version with that version manager, then try again."
        ),
        Refusal::InTemp => format!(
            "{tool} can't start: {file} is not used because it is in a temp folder or links to one, which agents or other users can change. Install it outside temp folders, then try again."
        ),
    }
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

/// Checks a found file: none when it is not an executable file whose links resolve, else
/// passed, or passed over as a version-manager shim, as a file agents can write, as found or
/// through its links ([`Places::agent_writable`]), with the part another user or group can
/// change ([`crate::service::changeable_past_sticky`]), or, on the engine's places, as a file
/// that leads into a temp folder ([`leads_into_temp`]). Inputs: the places and the file.
/// Output: the check.
fn check_candidate(places: &Places, candidate: &Path) -> Option<Result<(), Refusal>> {
    if !executable_file(candidate) {
        return None;
    }
    let real = candidate.canonicalize().ok()?;
    Some(if crate::harnesses::is_shim(places.home(), candidate) {
        Err(Refusal::Shim)
    } else if places.agent_writable(candidate) {
        Err(Refusal::AgentsCanWrite)
    } else if let Some(part) = crate::service::changeable_past_sticky(candidate) {
        Err(Refusal::OthersCanChange(part))
    } else if places.temp_refused && leads_into_temp(candidate, &real, &places.temps) {
        Err(Refusal::InTemp)
    } else {
        Ok(())
    })
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
