pub mod permission_fix;
pub mod probe;
#[cfg(test)]
pub(crate) mod stub;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(all(test, unix))]
mod tests;
#[cfg(all(test, windows))]
mod windows_tests;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// The engine answers program finding needs without naming the engine crate: harness
/// shims and prefixes, the projects the engine registered, and the folder checks that live
/// with the sandbox and the service until their own moves. Built once by crazyagent
/// (crazyagent's `HOOKS`). Inputs: the fields. Output: a value carried on [`Places`].
#[derive(Clone, Copy)]
pub struct FindingHooks {
    pub link_hops: fn(&Path) -> Option<Vec<PathBuf>>,
    pub home_places: fn(&Path) -> Vec<PathBuf>,
    pub in_crazyagent_folder: fn(&Path) -> bool,
    pub is_shim: fn(Option<&Path>, &Path) -> bool,
    pub npm_prefix: for<'a> fn(&'a Path) -> Option<&'a Path>,
    pub registered_projects: fn(&Path) -> Option<Vec<PathBuf>>,
    pub projects_marker: &'static str,
    pub data_twin: fn(&Path) -> Option<PathBuf>,
    pub one_spelling: fn(PathBuf) -> PathBuf,
    pub canonical: fn(&Path) -> std::io::Result<PathBuf>,
    pub in_agent_tmp: fn(&Path) -> bool,
    pub changeable_past_sticky: fn(&Path) -> Option<PathBuf>,
    pub changeable_by_others: fn(&Path) -> Option<PathBuf>,
    pub harness_prefix: fn(&Path) -> PathBuf,
    pub gate_root: fn() -> Option<PathBuf>,
}

impl std::fmt::Debug for FindingHooks {
    /// Prints the hooks without their addresses, which say nothing a reader can use.
    /// Input: the formatter. Output: the write result.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FindingHooks")
            .finish_non_exhaustive()
    }
}

/// What program finding needs from the engine: the hooks, the data folder the layout names
/// for places built with no engine root, and each harness's install folders under HOME.
/// Carried on every [`Places`], so a background folder run a resolve starts builds its
/// places the same way ([`probe::missed`]). Inputs: the fields. Output: the seed.
#[derive(Clone, Debug)]
pub struct FindingSeed {
    pub hooks: FindingHooks,
    pub layout_root: Option<PathBuf>,
    pub harness_locations: Vec<&'static str>,
}

impl PartialEq for FindingSeed {
    /// Compares every field but the hooks, which hold fn addresses that compare unreliably
    /// and never differ in production (crazyagent's `HOOKS`); the fields are named one by one, so a new
    /// one does not compile until it is compared. Input: the other seed. Output: the answer.
    fn eq(&self, other: &Self) -> bool {
        let Self {
            hooks: _,
            layout_root,
            harness_locations,
        } = self;
        *layout_root == other.layout_root && *harness_locations == other.harness_locations
    }
}

impl Eq for FindingSeed {}

const PLACES_MAX: usize = 160;
const NAME_BYTES_MAX: usize = 64;
const PATH_BYTES_MAX: usize = 1024;
const SCRIPT_HEAD_BYTES_MAX: usize = 256;
const SHEBANG_WORDS_MAX: usize = 8;
const ENV: &str = "/usr/bin/env";
const NAME_LAUNCHERS: [&str; 22] = [
    "env",
    "nice",
    "arch",
    "nohup",
    "time",
    "caffeinate",
    "xcrun",
    "script",
    "sandbox-exec",
    "timeout",
    "stdbuf",
    "setsid",
    "sudo",
    "doas",
    "busybox",
    "coreutils",
    "toybox",
    "genv",
    "gnice",
    "gnohup",
    "gtimeout",
    "gstdbuf",
];
pub const INTERPRETER_DEPTH_MAX: usize = 2;
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
pub const SYSTEM_FOLDERS: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
/// The folders whose tools stay usable while the set-up projects cannot be read
/// ([`Places::agent_writable`]): the system's, the Homebrew prefixes with their `Cellar` and
/// `opt` folders (`/usr/local` lies under `/usr`), and on Linux Linuxbrew's prefix, the Nix
/// store and the NixOS profiles. The normal path trusts every one of them too.
#[cfg(not(target_os = "linux"))]
const INSTALL_ROOTS: [&str; 4] = ["/usr", "/bin", "/sbin", "/opt/homebrew"];
#[cfg(target_os = "linux")]
const INSTALL_ROOTS: [&str; 8] = [
    "/usr",
    "/bin",
    "/sbin",
    "/opt/homebrew",
    "/home/linuxbrew/.linuxbrew",
    "/nix/store",
    "/run/current-system/sw",
    "/nix/var/nix/profiles/default",
];
/// The file name of the tmux the app ships beside the engine (DEC-157), which the name
/// `tmux` finds before every folder ([`Places::bundled`]).
pub const BUNDLED_TMUX: &str = "crazyagent-tmux";
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

/// The data root this process's engine recorded at its start ([`record_engine_root`]),
/// none in a process that runs no engine. No input. Output: the absolute root.
#[cfg(target_os = "linux")]
pub fn engine_root() -> Option<&'static Path> {
    let root = ENGINE_ROOT.get().map(PathBuf::as_path);
    assert!(root.is_none_or(Path::is_absolute));
    root
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
/// and every project the engine set up ([`FindingHooks::registered_projects`]) included; one in the engine's `setup` folder is set aside so [`resolve`] names a tool
/// there, and the engine's own places set aside a PATH or probed temp folder the same way
/// ([`Places::set_aside_temp`]). The app's folder is kept on its own for the tools the app
/// ships ([`Places::bundled`]).
#[derive(Clone, Debug)]
pub struct Places {
    folders: Vec<PathBuf>,
    home: Option<PathBuf>,
    engine_roots: Vec<PathBuf>,
    temp_refused: bool,
    temps: Vec<PathBuf>,
    refused_setup: Vec<PathBuf>,
    refused_temp: Vec<PathBuf>,
    app_folder: Option<PathBuf>,
    projects: Option<Vec<PathBuf>>,
    seed: FindingSeed,
}

impl PartialEq for Places {
    /// Compares every field; the seed compares without its hooks ([`FindingSeed`]). The
    /// fields are named one by one, so a new one does not compile until it is compared.
    /// Input: the other places. Output: the answer.
    fn eq(&self, other: &Self) -> bool {
        let Self {
            folders,
            home,
            engine_roots,
            temp_refused,
            temps,
            refused_setup,
            refused_temp,
            app_folder,
            projects,
            seed,
        } = self;
        *folders == other.folders
            && *home == other.home
            && *engine_roots == other.engine_roots
            && *temp_refused == other.temp_refused
            && *temps == other.temps
            && *refused_setup == other.refused_setup
            && *refused_temp == other.refused_temp
            && *app_folder == other.app_folder
            && *projects == other.projects
            && *seed == other.seed
    }
}

impl Eq for Places {}

impl Places {
    /// These places searching one folder only, for a file found beside another (npx's
    /// `npx-cli.js` beside `npx.cmd`). Input: the folder. Output: a copy whose search
    /// folders are that folder alone.
    #[cfg(windows)]
    pub(crate) fn only_in(&self, folder: &Path) -> Places {
        Places {
            folders: vec![folder.to_path_buf()],
            ..self.clone()
        }
    }

    /// Builds places from explicit folders and HOME, for a caller that names its own search
    /// space. Relative, oversized and repeated folders are dropped, and so is every folder
    /// inside a project or a `.crazyagent` folder. Inputs: the folders in order and HOME.
    /// Output: the bounded places.
    pub fn new(folders: Vec<PathBuf>, home: Option<PathBuf>, seed: FindingSeed) -> Self {
        Self::kept(folders, home, Vec::new(), false, Some(Vec::new()), seed)
    }

    /// Keeps the folders in order, dropping relative, oversized and repeated ones and every
    /// one agents can write, at most [`PLACES_MAX`], and sets aside one of those in an engine
    /// `setup` folder ([`Places::in_engine_setup`]). Inputs: the folders, HOME, the engine
    /// data folders, whether probed temp folders are refused and the set-up projects' folders,
    /// none when they could not be read, and the seed. Output: the bounded places.
    fn kept(
        folders: Vec<PathBuf>,
        home: Option<PathBuf>,
        engine_roots: Vec<PathBuf>,
        temp_refused: bool,
        projects: Option<Vec<PathBuf>>,
        seed: FindingSeed,
    ) -> Self {
        let hooks = seed.hooks;
        let mut places = Self {
            folders: Vec::new(),
            home: home.filter(|home| usable(home)),
            engine_roots: engine_roots
                .into_iter()
                .filter(|root| usable(root))
                .collect(),
            temp_refused,
            temps: if temp_refused {
                temp_roots(&hooks)
            } else {
                Vec::new()
            },
            refused_setup: Vec::new(),
            refused_temp: Vec::new(),
            app_folder: None,
            projects,
            seed,
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
    #[cfg(all(any(test, feature = "test-support"), unix))]
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
    /// fallback. A folder agents can write is never kept ([`Places::agent_writable`]); the
    /// app's folder is also kept for the tools it ships ([`Places::bundled`]). Inputs: the
    /// engine root, the app's folder, HOME, the PATH and the seed. Output: the places.
    pub fn engine(
        engine_root: Option<&Path>,
        app_folder: Option<&Path>,
        home: Option<PathBuf>,
        path: Option<&OsStr>,
        seed: FindingSeed,
    ) -> Self {
        let hooks = seed.hooks;
        let system: Vec<PathBuf> = SYSTEM_FOLDERS.iter().map(PathBuf::from).collect();
        let homebrew: Vec<PathBuf> = HOMEBREW_FOLDERS.iter().map(PathBuf::from).collect();
        let mut folders: Vec<PathBuf> = Vec::new();
        let mut temp: Vec<PathBuf> = Vec::new();
        folders.extend(app_folder.map(Path::to_path_buf));
        folders.extend(homebrew_folders(&hooks));
        if let Some(path) = path {
            for folder in
                std::env::split_paths(path).filter(|f| !system.contains(f) && !homebrew.contains(f))
            {
                if in_temp(&hooks, &folder) {
                    temp.push(folder);
                } else {
                    folders.push(folder);
                }
            }
        }
        if let Some(home) = home.as_deref().filter(|home| home.is_absolute()) {
            folders.extend((hooks.home_places)(home));
            folders.extend(node_version_folders(home));
            folders.extend(
                seed.harness_locations
                    .iter()
                    .map(|relative| below(home, relative)),
            );
        }
        #[cfg(target_os = "linux")]
        folders.extend(LINUX_FOLDERS.iter().map(PathBuf::from));
        folders.extend(system);
        folders.extend(
            engine_root
                .into_iter()
                .flat_map(|root| engine_folders(&hooks, root)),
        );
        Self {
            app_folder: app_folder
                .filter(|folder| usable(folder))
                .map(Path::to_path_buf),
            ..Self::kept(
                folders,
                home,
                engine_roots(&hooks, engine_root),
                true,
                Some(Vec::new()),
                seed,
            )
            .set_aside_temp(temp)
        }
    }

    /// Builds the engine's places from this process's HOME and PATH and the folder of the
    /// running executable. With no engine root given, a launch takes the one this process's
    /// engine recorded ([`record_engine_root`]), so it looks where setup does; with none,
    /// the data folder is still refused, though its setup folder is not looked in. Inputs:
    /// the engine root and the seed. Output: the places ([`Places::engine`]).
    pub fn from_environment(engine_root: Option<&Path>, seed: FindingSeed) -> Self {
        Self::at_roots(engine_root, ENGINE_ROOT.get().map(PathBuf::as_path), seed)
    }

    /// Builds the engine's places as [`Places::from_environment`] does, with the engine
    /// root this process recorded given explicitly so rows never record the global. With an
    /// engine root given or recorded, the set-up projects are read from its `engine.db` on
    /// every build, so a project set up after the engine started counts
    /// ([`FindingHooks::registered_projects`]); with neither, only the `.crazyagent` folder marks a project.
    /// Inputs: the engine root given, if any, the recorded one and the seed. Output: the
    /// places.
    fn at_roots(engine_root: Option<&Path>, recorded: Option<&Path>, seed: FindingSeed) -> Self {
        let engine_root = engine_root.or(recorded);
        let app = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.canonicalize().ok())
            .and_then(|exe| exe.parent().map(Path::to_path_buf));
        let hooks = seed.hooks;
        let probe_seed = seed.clone();
        let data_root = engine_root
            .map(Path::to_path_buf)
            .or_else(|| seed.layout_root.clone());
        Self {
            engine_roots: engine_roots(&hooks, data_root.as_deref()),
            projects: engine_root.map_or_else(|| Some(Vec::new()), hooks.registered_projects),
            ..Self::engine(
                engine_root,
                app.as_deref(),
                std::env::var_os("HOME").map(PathBuf::from),
                std::env::var_os("PATH").as_deref(),
                seed,
            )
        }
        .with_probed(probe::folders(|| probe_seed))
    }

    /// Adds the folders the background probes found ([`probe::folders`]), such as npm's
    /// global prefix and the login shell's PATH, after every folder known without running a
    /// program and before the system folders and the engine's own folders, which stay last;
    /// folders agents can write are dropped, and on the engine's places ([`Places::engine`])
    /// one in a temp folder, which agents with the owner's temp folder grant or other users
    /// write, is set aside ([`Places::set_aside_temp`]); under the clean-Mac gate root only
    /// folders in that root or HOME are kept ([`probed_kept`]). The app's folder stays as it
    /// was. Input: the folders. Output: the places.
    pub fn with_probed(self, probed: Vec<PathBuf>) -> Self {
        let probed = probed_kept(
            probed,
            gate_root(&self.seed.hooks).as_deref(),
            self.home.as_deref(),
        );
        let mut tail: Vec<PathBuf> = SYSTEM_FOLDERS.iter().map(PathBuf::from).collect();
        tail.extend(
            self.engine_roots
                .iter()
                .flat_map(|root| engine_folders(&self.seed.hooks, root)),
        );
        let temp_refused = self.temp_refused;
        let (last, mut folders): (Vec<PathBuf>, Vec<PathBuf>) = self
            .folders
            .into_iter()
            .partition(|folder| tail.contains(folder));
        let (temp, probed): (Vec<PathBuf>, Vec<PathBuf>) = probed
            .into_iter()
            .filter(|folder| !tail.contains(folder))
            .partition(|folder| temp_refused && in_temp(&self.seed.hooks, folder));
        folders.extend(probed);
        folders.extend(last);
        let set_aside = self.refused_setup;
        let carried = self.refused_temp;
        let app_folder = self.app_folder;
        let mut places = Self::kept(
            folders,
            self.home,
            self.engine_roots,
            temp_refused,
            self.projects,
            self.seed,
        );
        for folder in set_aside {
            if !places.refused_setup.contains(&folder) && places.refused_setup.len() < PLACES_MAX {
                places.refused_setup.push(folder);
            }
        }
        Self {
            app_folder,
            ..places.set_aside_temp(carried.into_iter().chain(temp))
        }
    }

    /// The folders looked in, in order. Output: the folders.
    pub fn folders(&self) -> &[PathBuf] {
        &self.folders
    }

    /// The copy of a tool the app ships beside the engine, looked at before every folder and
    /// checked as any file found: [`BUNDLED_TMUX`] in the app's folder for the name `tmux`
    /// (DEC-157), none for any other name or with no app folder. Input: the program name.
    /// Output: the file, which may not exist.
    pub fn bundled(&self, name: &str) -> Option<PathBuf> {
        let file = match name {
            "tmux" => BUNDLED_TMUX,
            _ => return None,
        };
        let bundled = self.app_folder.as_ref()?.join(file);
        assert!(bundled.is_absolute());
        Some(bundled)
    }

    /// The same places with no copy the app ships, for the usual order once that copy cannot
    /// run. Output: the places.
    pub fn without_bundled(&self) -> Self {
        Self {
            app_folder: None,
            ..self.clone()
        }
    }

    /// The same places with the app's folder set, as an engine started from that folder
    /// would hold it, for rows that build their own folders. Input: the app's folder.
    /// Output: the places.
    #[cfg(all(any(test, feature = "test-support"), unix))]
    pub(crate) fn with_app_folder(self, app_folder: PathBuf) -> Self {
        assert!(usable(&app_folder));
        Self {
            app_folder: Some(app_folder),
            ..self
        }
    }

    /// HOME, for the shim checks. Output: HOME, or none.
    pub fn home(&self) -> Option<&Path> {
        self.home.as_deref()
    }

    /// Whether a folder may be on the PATH of a program the engine starts, as a lookup here
    /// would trust its files: an absolute folder that exists, none agents can write
    /// ([`Places::agent_writable`]) or another user or group can change, no link on its way
    /// another can move ([`Places::link_hops_kept`]) and, on the engine's places, none in a
    /// temp folder (RV-DS241-157). Input: the folder. Output: the answer.
    pub fn kept_for_children(&self, folder: &Path) -> bool {
        folder.is_absolute()
            && folder.is_dir()
            && !self.agent_writable(folder)
            && (self.seed.hooks.changeable_past_sticky)(folder).is_none()
            && !(self.temp_refused && in_temp_of(&self.seed.hooks, folder, &self.temps))
            && self.link_hops_kept(folder)
    }

    /// Whether a path run as written meets only links no one else can move: every link and
    /// `..` folder on its way to the real file ([`FindingHooks::link_hops`]) is
    /// one agents cannot write, no other user or group can change and, on the engine's
    /// places, not one in a temp folder the path itself lies outside ([`leads_into_temp`]).
    /// Input: the absolute path. Output: the answer, false past the link bound.
    pub fn link_hops_kept(&self, path: &Path) -> bool {
        assert!(path.is_absolute());
        (self.seed.hooks.link_hops)(path)
            .is_some_and(|hops| self.hop_refusal(path, &hops).is_none())
    }

    /// Why a link or `..` folder on a path's way to its real file refuses the path, read as
    /// [`check_candidate`] reads the file itself: one agents can write, one another user or
    /// group can change past a sticky store or, on the engine's places, one in a temp folder
    /// the path lies outside ([`leads_into_temp`]). Inputs: the path as written and its hops
    /// ([`FindingHooks::link_hops`]). Output: the first refusal, or none.
    fn hop_refusal(&self, path: &Path, hops: &[PathBuf]) -> Option<Refusal> {
        if hops.iter().any(|hop| self.agent_writable(hop)) {
            return Some(Refusal::AgentsCanWrite);
        }
        let changeable = hops
            .iter()
            .find_map(|hop| (self.seed.hooks.changeable_past_sticky)(hop));
        if let Some(part) = changeable {
            return Some(Refusal::OthersCanChange(part));
        }
        let leads = |hop: &PathBuf| leads_into_temp(&self.seed.hooks, path, hop, &self.temps);
        (self.temp_refused && hops.iter().any(leads)).then_some(Refusal::InTemp)
    }

    /// Whether agents can write a path, read as written, resolved and in its one spelling
    /// ([`FindingHooks::canonical`]): in a `.crazyagent` folder, a project ([`in_project`]),
    /// a folder the engine names for agents, the engine's data folder outside `setup`, or an
    /// agent's short temp folder. HOME's names are read once per call. Input: the path.
    /// Output: the answer.
    pub fn agent_writable(&self, path: &Path) -> bool {
        let real = path.canonicalize().ok();
        let plain = (self.seed.hooks.canonical)(path).ok();
        let homes = home_names(&self.seed.hooks, self.home());
        let mut forms = std::iter::once(path)
            .chain(real.as_deref())
            .chain(plain.as_deref());
        forms.any(|path| {
            (self.seed.hooks.in_crazyagent_folder)(path)
                || in_project(&self.seed.hooks, &homes, path)
                || self.in_set_up_project(&homes, path)
                || in_engine_data(&self.engine_roots, path)
                || (self.seed.hooks.in_agent_tmp)(path)
        })
    }

    /// Whether a path lies in a folder the engine names for agents ([`FindingHooks::registered_projects`]),
    /// which they can write whole. When those could not be read it fails closed: every path
    /// outside the engine's data folder counts, under HOME and outside HOME too unless it
    /// lies under an install root ([`Places::installed`]) or the app's folder, since a
    /// repository the engine registered may lie anywhere (C2-H8). Inputs: HOME's names
    /// ([`home_names`]) and the path. Output: the answer.
    fn in_set_up_project(&self, homes: &[PathBuf], path: &Path) -> bool {
        assert!(homes.len() <= 4);
        let Some(roots) = &self.projects else {
            if self.engine_roots.iter().any(|root| path.starts_with(root)) {
                return false;
            }
            return homes.iter().any(|home| path.starts_with(home)) || !self.installed(path);
        };
        roots.iter().any(|root| path.starts_with(root))
    }

    /// Whether a path lies under an install root, in either of its macOS names
    /// ([`FindingHooks::data_twin`]), or in the app's folder: [`INSTALL_ROOTS`] on Unix and
    /// [`windows_install_roots`] on Windows. Input: the path. Output: the answer.
    fn installed(&self, path: &Path) -> bool {
        let roots: Vec<PathBuf> = if cfg!(windows) {
            windows_install_roots()
        } else {
            INSTALL_ROOTS.iter().map(PathBuf::from).collect()
        };
        let under = |root: &Path| {
            path.starts_with(root)
                || (self.seed.hooks.data_twin)(root).is_some_and(|twin| path.starts_with(twin))
        };
        roots.iter().any(|root| under(root))
            || self
                .app_folder
                .as_deref()
                .is_some_and(|folder| path.starts_with(folder))
    }
}

/// The Windows folders whose tools stay usable while the set-up projects cannot be read: the
/// engine's own `SystemRoot`, `ProgramFiles`, `ProgramFiles(x86)` and `ProgramW6432`, each
/// kept only when it names an absolute folder. Output: the folders, at most 4.
fn windows_install_roots() -> Vec<PathBuf> {
    let roots: Vec<PathBuf> = [
        "SystemRoot",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
    ]
    .into_iter()
    .filter_map(std::env::var_os)
    .map(PathBuf::from)
    .filter(|root| root.is_absolute())
    .collect();
    assert!(roots.len() <= 4);
    roots
}

/// HOME's names: as given, with its links resolved, in its one spelling and, on macOS, that
/// spelling's data volume name, so a path in either spelling meets HOME in its own. Input:
/// the hooks and HOME, or none. Output: the names, each once.
fn home_names(hooks: &FindingHooks, home: Option<&Path>) -> Vec<PathBuf> {
    let Some(home) = home else {
        return Vec::new();
    };
    let plain = (hooks.canonical)(home).ok();
    let twin = plain.as_deref().and_then(hooks.data_twin);
    let mut names = vec![home.to_path_buf()];
    for name in [home.canonicalize().ok(), plain, twin]
        .into_iter()
        .flatten()
    {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    assert!(names.len() <= 4);
    names
}

/// Whether a path lies in the engine's data folder outside its `setup` folder, where the
/// agents' HOMEs, worktrees and team state live, which agents write. The one rule for the
/// resolver and the harness pins. Inputs: the data folder as given and real
/// ([`engine_roots`]) and the path. Output: the answer.
pub fn in_engine_data(roots: &[PathBuf], path: &Path) -> bool {
    roots
        .iter()
        .any(|root| path.starts_with(root) && !path.starts_with(root.join("setup")))
}

/// The engine's own folders, looked in after every folder of the owner: its setup `bin`,
/// then its npm harness prefix's `bin` ([`FindingHooks::harness_prefix`]). Input: the engine
/// root and the hooks. Output: the two folders in that order.
fn engine_folders(hooks: &FindingHooks, root: &Path) -> [PathBuf; 2] {
    [
        root.join("setup").join("bin"),
        (hooks.harness_prefix)(root).join("bin"),
    ]
}

/// The engine data folder as given, with its links resolved and in its one spelling
/// ([`FindingHooks::canonical`]), so a path in the short spelling meets a data folder given
/// through the data volume. Inputs: the hooks and the folder, or none. Output: the
/// folders, each once.
pub fn engine_roots(hooks: &FindingHooks, root: Option<&Path>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = root.map(Path::to_path_buf).into_iter().collect();
    let resolved = root
        .into_iter()
        .flat_map(|root| [root.canonicalize().ok(), (hooks.canonical)(root).ok()]);
    for real in resolved.flatten() {
        if !roots.contains(&real) {
            roots.push(real);
        }
    }
    assert!(roots.len() <= 3);
    roots
}

/// Whether a path is inside a CrazyAgent project: it or a folder above it, other than HOME
/// under any of its names ([`home_names`]) and `/`, holds a `.crazyagent` entry, which the
/// sandbox never lets an agent remove. A path deeper than [`ANCESTORS_MAX`] counts as
/// inside. Inputs: the hooks, HOME's names and the path. Output: the answer.
fn in_project(hooks: &FindingHooks, homes: &[PathBuf], path: &Path) -> bool {
    assert!(homes.len() <= 4);
    if path.components().count() > ANCESTORS_MAX {
        return true;
    }
    path.ancestors()
        .filter(|folder| {
            folder.parent().is_some() && !homes.iter().any(|home| home.as_path() == *folder)
        })
        .any(|folder| std::fs::symlink_metadata(folder.join(hooks.projects_marker)).is_ok())
}

/// Whether a file found through a place outside every temp folder really lies in one, as an
/// owner link from `~/.local/bin` into the temp folder does, which agents with the owner's
/// temp folder grant can replace; a file whose place is itself in a temp folder (a test HOME,
/// an engine root, a mounted app) does not. Inputs: the hooks, the file as written, its real
/// path and the temp folders. Output: the answer.
pub fn leads_into_temp(
    hooks: &FindingHooks,
    written: &Path,
    real: &Path,
    temps: &[PathBuf],
) -> bool {
    !under_temp(hooks, written, temps) && under_temp(hooks, real, temps)
}

/// Whether a path lies under one of the temp folders given, as written or in its one spelling,
/// so a macOS path named through the data volume still matches its short-named temp folder.
/// Inputs: the hooks, the path and the temp folders. Output: the answer.
fn under_temp(hooks: &FindingHooks, path: &Path, temps: &[PathBuf]) -> bool {
    let short = (hooks.one_spelling)(path.to_path_buf());
    temps
        .iter()
        .any(|temp| path.starts_with(temp) || short.starts_with(temp))
}

/// The temp folders: the shared ones and this user's own, as given, with its links resolved
/// and in the plain form [`FindingHooks::canonical`] gives (a Windows drive path without
/// `\\?\`), so a path matches however it is written; an owner temp folder of `/` is
/// dropped. Input: the hooks. Output: the absolute folders, each once.
pub fn temp_roots(hooks: &FindingHooks) -> Vec<PathBuf> {
    let owner = std::env::temp_dir();
    let real = owner.canonicalize().ok();
    let plain = (hooks.canonical)(&owner).ok();
    let mut roots: Vec<PathBuf> = TEMP_FOLDERS.iter().map(PathBuf::from).collect();
    for root in std::iter::once(owner).chain(real).chain(plain) {
        if root.is_absolute() && root.parent().is_some() && !roots.contains(&root) {
            roots.push(root);
        }
    }
    assert!(roots.len() <= TEMP_FOLDERS.len() + 3);
    roots
}

/// Whether a folder is in a temp folder, which other users, or agents with the temp folder
/// grant, can write: this user's temp folder or a shared one, as written and with its links
/// resolved. Inputs: the hooks and the folder. Output: the answer.
fn in_temp(hooks: &FindingHooks, folder: &Path) -> bool {
    in_temp_of(hooks, folder, &temp_roots(hooks))
}

/// Whether a folder is in one of the temp folders given, as written and with its links
/// resolved, in either macOS spelling; none given refuses nothing. Inputs: the hooks, the
/// folder and the temp folders. Output: the answer.
pub fn in_temp_of(hooks: &FindingHooks, folder: &Path, temps: &[PathBuf]) -> bool {
    if temps.is_empty() {
        return false;
    }
    let real = folder.canonicalize().ok();
    std::iter::once(folder)
        .chain(real.as_deref())
        .any(|folder| under_temp(hooks, folder, temps))
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
    /// that is itself a script, which macOS does not run, and every script on Windows, which
    /// runs no first line. Output: the absolute words.
    pub fn argv(&self) -> Vec<OsString> {
        let words: Vec<OsString> = match &self.kind {
            Kind::Native => vec![self.program.clone().into_os_string()],
            Kind::Script {
                interpreter,
                by_name: false,
                ..
            } if cfg!(unix) && interpreter.kind == Kind::Native => {
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
/// grant or other users write, a script whose first line the resolver cannot read the
/// way the system and `/usr/bin/env` run it, or a Windows batch file that is neither an npm
/// cmd-shim nor npm's own `npx.cmd`, which std would start through cmd.exe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    OthersCanChange(PathBuf),
    AgentsCanWrite,
    Shim,
    InTemp,
    FirstLine,
    Batch,
}

/// A file the resolver found and passed over, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub file: PathBuf,
    pub why: Refusal,
}

/// A tool that cannot run: which one, why, what it needs (the missing interpreter, or the
/// version needed when the one found is too old), the version found when that is too old,
/// the folders looked in and the first file passed over when no usable one was found,
/// boxed so the error stays small, kept apart from the owner line so it is fixed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    pub tool: String,
    pub reason: Reason,
    pub needs: Option<String>,
    pub found: Option<String>,
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
            (Reason::WrongVersion, tool, needs) => match (self.found.as_deref(), needs) {
                (Some(found), Some(needs)) => format!(
                    "{tool} {found} is too old for CrazyAgent; it needs {needs} or newer. Update it, then try again."
                ),
                _ => format!("{tool} is too old for CrazyAgent. Update it, then try again."),
            },
            (Reason::Refused, tool, _) => match &self.refused {
                Some(refused) => refused_line(tool, refused),
                None => format!(
                    "{tool} can't start: the file found for it is not used, since others can change it. Check who can write it, then try again."
                ),
            },
        }
    }
}

/// Finds a tool by its program name, first the copy the app ships ([`Places::bundled`]),
/// then wherever the places look, passing over version-manager shims, files agents can
/// write and files others can change, then reads how it runs; a miss names the first file
/// passed over ([`Refused`]), a set-aside `setup` folder's first, and asks the background
/// probes to look again ([`probe::missed`]). Harness launches call [`resolve_entry`].
/// Inputs: the program name and the places. Output: the tool, or why it cannot run.
pub fn resolve(name: &str, places: &Places) -> Result<Resolved, Missing> {
    resolve_at(name, places, 0)
}

/// Whether this build ships tmux beside the engine: the release job sets
/// `CRAZYAGENT_BUNDLES_TMUX=1` for every target that bundles `crazyagent-tmux`, so a dev
/// build or a Windows build never says the bundled one is missing.
pub const SHIPS_BUNDLED_TMUX: bool = match option_env!("CRAZYAGENT_BUNDLES_TMUX") {
    Some(mark) => matches!(mark.as_bytes(), [b'1']),
    None => false,
};

/// Why the engine runs another tmux than the one the app ships (DEC-157 15:41): the bundled
/// file is not there, or it is there but cannot run (refused, or no supported version read).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BundleNote {
    Missing,
    CannotRun,
}

impl BundleNote {
    /// The owner line the setup row shows under the tmux in use, word for word in the TUI
    /// and the desktop app. Output: the line.
    pub fn line(self) -> &'static str {
        match self {
            Self::Missing => {
                "Using the system tmux, the bundled one is missing; reinstall CrazyAgent."
            }
            Self::CannotRun => {
                "Using the system tmux, the bundled one can't run; reinstall CrazyAgent."
            }
        }
    }
}

/// Finds the tmux the engine uses, the one answer for setup, the terminal and the sandbox
/// grant: the bundled copy when it passes the entry checks and `runs`, else the usual
/// order with the reason the bundled one was not used; with no other tmux, the usual
/// search's own miss. Inputs: the places, whether this build ships tmux and the check that
/// the bundled copy runs. Output: the tmux and its note, or why none can run.
pub fn resolve_tmux(
    places: &Places,
    ships_bundle: bool,
    runs: impl Fn(&Resolved) -> bool,
) -> Result<(Resolved, Option<BundleNote>), Missing> {
    let Some(bundled) = places.bundled("tmux") else {
        return resolve("tmux", places).map(|found| (found, None));
    };
    let note = if std::fs::symlink_metadata(&bundled).is_ok() {
        Some(BundleNote::CannotRun)
    } else if ships_bundle {
        Some(BundleNote::Missing)
    } else {
        None
    };
    match resolve("tmux", places) {
        Ok(found) if found.entry == bundled && runs(&found) => Ok((found, None)),
        Ok(found) if found.entry != bundled => Ok((found, note)),
        _ => resolve("tmux", &places.without_bundled()).map(|found| {
            assert_ne!(found.entry, bundled);
            (found, note)
        }),
    }
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
        found: None,
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
    let found = places
        .bundled(name)
        .into_iter()
        .chain(places.folders.iter().map(|folder| folder.join(name)))
        .find(|candidate| match check_candidate(places, candidate) {
            Some(Ok(())) => true,
            Some(Err(why)) => {
                refused.get_or_insert_with(|| Refused {
                    file: candidate.clone(),
                    why,
                });
                false
            }
            None => false,
        });
    let Some(found) = found else {
        probe::missed(|| places.seed.clone());
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

pub(crate) fn entry_at(entry: &Path, places: &Places, depth: usize) -> Result<Resolved, Missing> {
    let tool = entry
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("the program")
        .to_owned();
    let not_there = || Missing {
        tool: tool.clone(),
        reason: Reason::NotFound,
        needs: None,
        found: None,
        looked: entry.parent().map(Path::to_path_buf).into_iter().collect(),
        refused: None,
    };
    let program = entry
        .canonicalize()
        .ok()
        .filter(|_| entry.is_absolute())
        .ok_or_else(not_there)?;
    #[cfg(any(test, feature = "test-support"))]
    if let Some(step) = between_program_and_check(entry) {
        step();
    }
    let refused = |file: &Path, why: Refusal| Missing {
        reason: Reason::Refused,
        refused: Some(Box::new(Refused {
            file: file.to_path_buf(),
            why,
        })),
        ..not_there()
    };
    match check_candidate(places, entry) {
        Some(Ok(())) => {}
        Some(Err(why)) => return Err(refused(entry, why)),
        None => return Err(not_there()),
    }
    match check_candidate(places, &program) {
        Some(Ok(())) if program.canonicalize().ok().as_ref() == Some(&program) => {}
        Some(Err(why)) => return Err(refused(program.as_path(), why)),
        _ => return Err(not_there()),
    }
    if places.temp_refused && leads_into_temp(&places.seed.hooks, entry, &program, &places.temps) {
        return Err(refused(program.as_path(), Refusal::InTemp));
    }
    assert!(program.is_absolute());
    #[cfg(windows)]
    if let Some(mapped) = crate::batch::entry(entry, &program, places, depth, refused) {
        return mapped;
    }
    let shebang = match shebang(&program) {
        Ok(Some(shebang)) => shebang,
        Ok(None) => {
            return Ok(Resolved {
                entry: entry.to_path_buf(),
                program,
                kind: Kind::Native,
            });
        }
        Err(Unreadable) => return Err(refused(program.as_path(), Refusal::FirstLine)),
    };
    let needs = |name: &str| Missing {
        tool: tool.clone(),
        reason: Reason::NeedsInterpreter,
        needs: Some(name.to_owned()),
        found: None,
        looked: places.folders.clone(),
        refused: None,
    };
    let interpreter_name = shebang.interpreter_name();
    if depth == INTERPRETER_DEPTH_MAX {
        return Err(needs(&interpreter_name));
    }
    let through_shim = shebang.by_name.is_none()
        && (places.seed.hooks.is_shim)(places.home(), &shebang.interpreter);
    let into_temp = places.temp_refused
        && shebang
            .interpreter
            .canonicalize()
            .is_ok_and(|real| leads_into_temp(&places.seed.hooks, entry, &real, &places.temps));
    let interpreter = match &shebang.by_name {
        Some(name) => named_interpreter(name, &program, places, depth + 1),
        None if through_shim => match check_place(places, &shebang.interpreter) {
            Some(Ok(())) => resolve_at(&interpreter_name, places, depth + 1),
            Some(Err(why)) => Err(refused(shebang.interpreter.as_path(), why)),
            None => Err(not_there()),
        },
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
    let real_name = interpreter.program.file_name().and_then(OsStr::to_str);
    if real_name.is_some_and(launches_by_name) || launches_by_name(&interpreter_name) {
        return Err(refused(program.as_path(), Refusal::FirstLine));
    }
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

/// A step a row runs once in `entry_at` between reading an entry's real file and checking
/// the entry.
#[cfg(any(test, feature = "test-support"))]
pub(crate) type Step = Box<dyn FnOnce() + Send>;

/// The steps rows set, each by the entry whose resolution runs it.
#[cfg(any(test, feature = "test-support"))]
static BETWEEN_PROGRAM_AND_CHECK: std::sync::Mutex<Vec<(PathBuf, Step)>> =
    std::sync::Mutex::new(Vec::new());

/// Takes the step a row set for this entry, to run in `entry_at` once the entry's real file
/// was read and before the entry is checked, so a row can move a link of its chain in that
/// gap. Input: the entry. Output: the step, none for every other entry.
#[cfg(any(test, feature = "test-support"))]
fn between_program_and_check(entry: &Path) -> Option<Step> {
    let mut steps = BETWEEN_PROGRAM_AND_CHECK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let at = steps.iter().position(|(path, _)| path == entry)?;
    Some(steps.remove(at).1)
}

/// Finds the interpreter a script's first line asks `env` for by name: for `node`, first the
/// one in its own npm prefix ([`FindingHooks::npm_prefix`]) when the resolver's checks
/// pass it, else the places in order, as `<name>.exe` on Windows; with none there, why that
/// own `node` was passed over, so the owner is told the file to fix, not to install a node.
/// Inputs: the name, the script's real file, the places and the depth. Output: it, or why.
fn named_interpreter(
    name: &str,
    script: &Path,
    places: &Places,
    depth: usize,
) -> Result<Resolved, Missing> {
    assert!(depth <= INTERPRETER_DEPTH_MAX);
    let mut passed = None;
    if name == "node"
        && let Some((node, judged)) = own_node(script, places, depth)
    {
        match judged {
            Ok(found) => return Ok(found),
            Err(missing) => {
                log_own_node_passed_over(&node, &missing);
                passed = Some(missing);
            }
        }
    }
    #[cfg(windows)]
    let name = &format!("{name}.exe");
    match (resolve_at(name, places, depth), passed) {
        (Err(none), Some(passed)) if none.reason == Reason::NotFound => Err(passed),
        (found, _) => found,
    }
}

/// The `node` file in a script's own npm prefix ([`FindingHooks::npm_prefix`]) and how
/// the resolver's checks judge it at the depth given: none when the script has no prefix or
/// the prefix has no `node`, else that `node` or why it is passed over. Inputs: the script's
/// real file, the places and the depth. Output: the file and its judgement, or none.
fn own_node(
    script: &Path,
    places: &Places,
    depth: usize,
) -> Option<(PathBuf, Result<Resolved, Missing>)> {
    assert!(depth <= INTERPRETER_DEPTH_MAX);
    assert!(script.is_absolute());
    let node = (places.seed.hooks.npm_prefix)(script)?
        .join("bin")
        .join("node");
    let judged = entry_at(&node, places, depth);
    if judged
        .as_ref()
        .is_err_and(|missing| missing.reason == Reason::NotFound)
    {
        return None;
    }
    Some((node, judged))
}

/// Why a resolved script whose first line asks `env` for `node` would start under a `node`
/// other than its own npm prefix's: why the resolver passes that one over, named for the
/// tool, so a harness launch refuses where npm falls back (V243-OWN-PREFIX-NODE-REFUSE). The
/// tool is resolved at depth 0, so its `node` is judged at depth 1, as [`named_interpreter`]
/// judges it. Inputs: the tool and the places. Output: why, or none.
pub fn own_node_refused(tool: &Resolved, places: &Places) -> Option<Missing> {
    assert!(tool.program.is_absolute());
    if !matches!(tool.kind, Kind::Script { by_name: true, .. }) {
        return None;
    }
    let asks_node = shebang(&tool.program)
        .ok()
        .flatten()
        .is_some_and(|first| first.by_name.as_deref() == Some("node"));
    if !asks_node {
        return None;
    }
    let passed = own_node(&tool.program, places, 1)?.1.err()?;
    let name = tool
        .entry
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("the program");
    Some(Missing {
        tool: name.to_owned(),
        ..passed
    })
}

/// The most own-prefix `node` files the engine log names, each once per process.
const OWN_NODE_LOGGED_MAX: usize = 64;

/// Writes one engine log line for an npm prefix's own `node` the resolver found and passed
/// over, with its reason, once per file per process and for at most [`OWN_NODE_LOGGED_MAX`]
/// files, so the fallback to the places' `node` is never silent (G1 P7); a harness launch
/// refuses instead ([`own_node_refused`]). Inputs: the file and why it was passed over.
/// Output: whether this call wrote the line.
fn log_own_node_passed_over(node: &Path, passed: &Missing) -> bool {
    static LOGGED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
    assert_ne!(passed.reason, Reason::NotFound);
    assert!(node.is_absolute());
    let mut logged = LOGGED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if logged.len() == OWN_NODE_LOGGED_MAX || logged.iter().any(|seen| seen == node) {
        return false;
    }
    logged.push(node.to_path_buf());
    assert!(logged.len() <= OWN_NODE_LOGGED_MAX);
    eprintln!(
        "tools_own_node_passed_over file={} reason={} refused={:?}",
        node.display(),
        passed.reason.code(),
        passed.refused.as_deref().map(|refused| &refused.why)
    );
    true
}

/// Whether a name is one of a launcher that starts the program it is given looked up by name
/// in its own PATH, such as `env`, `nice`, `arch` or `nohup`, so a first line running it
/// starts a copy the resolver never judged (rv-sec2 B11 r2 F2). [`entry_at`] asks it of the
/// interpreter's real file name and of the name the line runs it as, its program word for
/// `env` or the file name as written, since a multi-call binary such as coreutils or busybox
/// picks its tool by that name (rv-code4 B18 F1). On Windows the name is read as Windows
/// finds a program, ASCII case ignored and a trailing `.exe` dropped (#333 r1 sec F1).
/// Input: the name. Output: the answer.
fn launches_by_name(name: &str) -> bool {
    #[cfg(windows)]
    let lower = name.to_ascii_lowercase();
    #[cfg(windows)]
    let name = lower.strip_suffix(".exe").unwrap_or(&lower);
    NAME_LAUNCHERS.contains(&name)
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

/// A first line the resolver cannot read the way the system and `/usr/bin/env` run it, so
/// what it starts is not known.
struct Unreadable;

/// Reads the `#!` line of a file the way the system runs it, at most
/// [`SCRIPT_HEAD_BYTES_MAX`] bytes, ended by a newline or a NUL, its words split on spaces
/// and tabs: an absolute interpreter with its words, or `/usr/bin/env`, after one `-S` at
/// most, with the program it names. An interpreter that is `env` anywhere else is refused
/// here, and one whose real file launches by name by the resolver ([`entry_at`]). A file
/// it cannot read, which the system still runs, and a line holding `#`, where the system
/// ends it, are [`Unreadable`] (rv-sec2 B11 r2 F3, F4). Input: the real file. Output: the
/// line, none for a file that is not a script, or [`Unreadable`] for any other first line.
fn shebang(file: &Path) -> Result<Option<Shebang>, Unreadable> {
    use std::io::Read;
    let mut head = Vec::with_capacity(SCRIPT_HEAD_BYTES_MAX);
    let opened = std::fs::File::open(file).map_err(|_| Unreadable)?;
    if opened
        .take(SCRIPT_HEAD_BYTES_MAX as u64)
        .read_to_end(&mut head)
        .is_err()
    {
        return Err(Unreadable);
    }
    assert!(head.len() <= SCRIPT_HEAD_BYTES_MAX);
    let Some(line) = head.strip_prefix(b"#!") else {
        return Ok(None);
    };
    let end = line.iter().position(|byte| *byte == b'\n' || *byte == 0);
    if end.is_none() && head.len() == SCRIPT_HEAD_BYTES_MAX {
        return Err(Unreadable);
    }
    let line = std::str::from_utf8(&line[..end.unwrap_or(line.len())]).map_err(|_| Unreadable)?;
    if line
        .chars()
        .any(|c| (c.is_control() && c != '\t') || c == '#')
    {
        return Err(Unreadable);
    }
    let words: Vec<&str> = line
        .split([' ', '\t'])
        .filter(|word| !word.is_empty())
        .collect();
    if words.len() > SHEBANG_WORDS_MAX {
        return Err(Unreadable);
    }
    let (first, rest) = words.split_first().ok_or(Unreadable)?;
    let interpreter = PathBuf::from(first);
    if interpreter != Path::new(ENV) {
        if !usable(&interpreter) || interpreter.file_name() == Some(OsStr::new("env")) {
            return Err(Unreadable);
        }
        return Ok(Some(Shebang {
            interpreter,
            by_name: None,
            arguments: rest.iter().map(|word| (*word).to_owned()).collect(),
        }));
    }
    let split = rest.first() == Some(&"-S");
    let rest = if split { &rest[1..] } else { rest };
    let (program, arguments) = rest.split_first().ok_or(Unreadable)?;
    let plain = !program.starts_with('-')
        && !program.contains('=')
        && !(split && program.contains(['$', '\\', '"', '\'']));
    if !plain {
        return Err(Unreadable);
    }
    let arguments: Vec<String> = arguments.iter().map(|word| (*word).to_owned()).collect();
    if program.starts_with('/') {
        let interpreter = PathBuf::from(program);
        if !usable(&interpreter) {
            return Err(Unreadable);
        }
        return Ok(Some(Shebang {
            interpreter,
            by_name: None,
            arguments,
        }));
    }
    if !valid_name(program) {
        return Err(Unreadable);
    }
    Ok(Some(Shebang {
        interpreter,
        by_name: Some((*program).to_owned()),
        arguments,
    }))
}

/// Homebrew's program folders on Apple silicon and Intel. Only a build with the
/// `clean-mac-gate` feature, never a release, reads them under the gate's root
/// ([`gate_root`]), so the clean-Mac gate sees none of this computer's Homebrew tools.
/// Input: the hooks. Output: the two folders.
pub fn homebrew_folders(hooks: &FindingHooks) -> Vec<PathBuf> {
    let root = gate_root(hooks);
    HOMEBREW_FOLDERS
        .iter()
        .map(|folder| match &root {
            Some(root) => below(root, folder.trim_start_matches('/')),
            None => PathBuf::from(folder),
        })
        .collect()
}

/// The probed folders a build looks in: every one in a release; under the clean-Mac
/// gate's stand-in system root, only those inside that root or HOME, since the login
/// shell also reads this computer's own /etc files and can name its real Homebrew.
/// Inputs: the probed folders, the gate's root and HOME. Output: the folders kept.
fn probed_kept(
    probed: Vec<PathBuf>,
    gate_root: Option<&Path>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let Some(root) = gate_root else {
        return probed;
    };
    probed
        .into_iter()
        .filter(|folder| {
            folder.starts_with(root) || home.is_some_and(|home| folder.starts_with(home))
        })
        .collect()
}

/// The clean-Mac gate's stand-in system root the engine names ([`FindingHooks::gate_root`]),
/// kept only when it is a usable folder. Input: the hooks. Output: the root, or none, as in
/// every release.
fn gate_root(hooks: &FindingHooks) -> Option<PathBuf> {
    (hooks.gate_root)().filter(|root| usable(root))
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
        Refusal::FirstLine => format!(
            "{tool} can't start: {file} is not used because its first line could run a program CrazyAgent can't check (a relative interpreter, env with a setting or a flag other than -S, a launcher such as nice or nohup that looks a program up by name, a # in the line, a line that is too long, or a file CrazyAgent can't read). Name the interpreter by its full path, or as /usr/bin/env and a program name, then try again."
        ),
        Refusal::Batch => format!(
            "{tool} can't start: {file} is not used because it is a Windows batch file, which runs through cmd.exe. CrazyAgent reads only a .cmd that npm made for a Node.js program, or npm's own npx.cmd beside node.exe. Use the program's .exe, or install it again with npm, then try again."
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
/// passed, or passed over as a version-manager shim, or as [`check_place`] refuses it.
/// Inputs: the places and the file. Output: the check.
fn check_candidate(places: &Places, candidate: &Path) -> Option<Result<(), Refusal>> {
    let place = check_place(places, candidate)?;
    Some(if (places.seed.hooks.is_shim)(places.home(), candidate) {
        Err(Refusal::Shim)
    } else {
        place
    })
}

/// Checks where a file sits, shim or not: none when it is not an executable file whose links
/// resolve, else passed, or passed over as agents' to write, as changeable by another user
/// or group past a sticky store, or as leading into a temp folder: the file as found and
/// real first, then each link and `..` folder on its way, as it runs by its path
/// ([`Places::hop_refusal`], B1). Inputs: the places and the file. Output: the check.
pub fn check_place(places: &Places, candidate: &Path) -> Option<Result<(), Refusal>> {
    if !executable_file(candidate) {
        return None;
    }
    let real = candidate.canonicalize().ok()?;
    let hops = (places.seed.hooks.link_hops)(candidate)?;
    Some(if places.agent_writable(candidate) {
        Err(Refusal::AgentsCanWrite)
    } else if let Some(part) = (places.seed.hooks.changeable_past_sticky)(candidate) {
        Err(Refusal::OthersCanChange(part))
    } else if places.temp_refused
        && leads_into_temp(&places.seed.hooks, candidate, &real, &places.temps)
    {
        Err(Refusal::InTemp)
    } else if let Some(refusal) = places.hop_refusal(candidate, &hops) {
        Err(refusal)
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

/// Whether a path is a file the run's user can execute, as the system decides at exec, so a
/// candidate or interpreter the engine's user cannot start never resolves, and a copy `env`
/// passes over is never one a walk takes and passes (rv-sec3 B18 F1). Input: the path.
/// Output: the answer.
#[cfg(unix)]
fn executable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
        && rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
}

/// Whether a path is a file, where the platform has no execute permission to test.
/// Input: the path. Output: the answer.
#[cfg(not(unix))]
fn executable_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}
