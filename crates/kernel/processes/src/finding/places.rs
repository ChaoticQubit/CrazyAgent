use super::*;

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
        Self::kept(
            folders,
            Vec::new(),
            (home, None),
            Vec::new(),
            false,
            (Some(Vec::new()), Vec::new()),
            seed,
        )
    }

    /// Keeps the folders in order, then the fallback folders, dropping relative, oversized
    /// and repeated ones and every one agents can write, at most [`PLACES_MAX`], and sets
    /// aside one of those in an engine `setup` folder ([`Places::in_engine_setup`]). The
    /// ordinary folders fill only the room the fallback folders leave, so those are always
    /// kept, last. The app's folder is set first, so the checks count it as installed
    /// ([`Places::installed`]). Inputs: the folders, the fallback folders (the system's, then
    /// the engine's), HOME and the app's folder, the engine data folders, whether probed temp
    /// folders are refused, the set-up projects' folders, none when they could not be read,
    /// with the last good read's folders kept for that case, and the seed. Output: the
    /// bounded places.
    pub(super) fn kept(
        folders: Vec<PathBuf>,
        tail: Vec<PathBuf>,
        (home, app_folder): (Option<PathBuf>, Option<PathBuf>),
        engine_roots: Vec<PathBuf>,
        temp_refused: bool,
        (projects, last_good): (Option<Vec<PathBuf>>, Vec<PathBuf>),
        seed: FindingSeed,
    ) -> Self {
        assert!(projects.is_none() || last_good.is_empty());
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
            app_folder: app_folder.filter(|folder| usable(folder)),
            projects,
            last_good,
            seed,
        };
        assert!(tail.len() <= SYSTEM_FOLDERS.len() + 3 * 2);
        let room = PLACES_MAX - tail.len();
        let ordinary = folders.into_iter().map(|folder| (folder, room));
        let last = tail.into_iter().map(|folder| (folder, PLACES_MAX));
        for (folder, cap) in ordinary.chain(last) {
            if places.folders.len() == cap {
                assert!(cap == room);
                continue;
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
    pub(super) fn with_temps(mut self, temps: Vec<PathBuf>) -> Self {
        assert!(self.temp_refused);
        self.temps = temps;
        self
    }

    /// Sets aside temp folders the engine's places refuse, never looked in, so [`resolve`]
    /// names a tool found only there instead of calling it missing; at most [`PLACES_MAX`].
    /// Input: the folders, absolute ones kept. Output: the places.
    pub(super) fn set_aside_temp(mut self, folders: impl IntoIterator<Item = PathBuf>) -> Self {
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
    pub(super) fn in_engine_setup(&self, folder: &Path) -> bool {
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
        let mut tail = system;
        tail.extend(
            engine_root
                .into_iter()
                .flat_map(|root| engine_folders(&hooks, root)),
        );
        Self::kept(
            folders,
            tail,
            (home, app_folder.map(Path::to_path_buf)),
            engine_roots(&hooks, engine_root),
            true,
            (Some(Vec::new()), Vec::new()),
            seed,
        )
        .set_aside_temp(temp)
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
    /// ([`FindingHooks::registered_projects`]); a failed read keeps that data folder's last
    /// good read ([`last_good_read`]); with neither, only the `.crazyagent` folder marks a project.
    /// Inputs: the engine root given, if any, the recorded one and the seed. Output: the
    /// places.
    pub(super) fn at_roots(
        engine_root: Option<&Path>,
        recorded: Option<&Path>,
        seed: FindingSeed,
    ) -> Self {
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
        let (projects, last_good) = match engine_root {
            Some(root) => last_good_read(&hooks, root, (hooks.registered_projects)(root)),
            None => (Some(Vec::new()), Vec::new()),
        };
        Self {
            engine_roots: engine_roots(&hooks, data_root.as_deref()),
            projects,
            last_good,
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
        let set_aside = self.refused_setup;
        let carried = self.refused_temp;
        let mut places = Self::kept(
            folders,
            last,
            (self.home, self.app_folder),
            self.engine_roots,
            temp_refused,
            (self.projects, self.last_good),
            self.seed,
        );
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

    /// The copy of a tool the app ships beside the engine, looked at before every folder and
    /// checked as any file found: [`BUNDLED_TMUX`] in the app's folder for the name `tmux`,
    /// none for any other name or with no app folder. Input: the program name. Output: the
    /// file, which may not exist.
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
    /// temp folder. Input: the folder. Output: the answer.
    pub fn kept_for_children(&self, folder: &Path) -> bool {
        folder.is_absolute()
            && folder.is_dir()
            && !self.agent_writable(folder)
            && crate::service::changeable_past_sticky(folder).is_none()
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
    pub(super) fn hop_refusal(&self, path: &Path, hops: &[PathBuf]) -> Option<Refusal> {
        if hops.iter().any(|hop| self.agent_writable(hop)) {
            return Some(Refusal::AgentsCanWrite);
        }
        let changeable = hops
            .iter()
            .find_map(|hop| crate::service::changeable_past_sticky(hop));
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
    /// under a folder the last good read named counts, then every path outside the engine's
    /// data folder, under HOME and outside HOME too unless it lies under an install root
    /// ([`Places::installed`]) or the app's folder, since a repository the engine registered
    /// may lie anywhere. Inputs: HOME's names ([`home_names`]) and the path. Output: the
    /// answer.
    pub(super) fn in_set_up_project(&self, homes: &[PathBuf], path: &Path) -> bool {
        assert!(homes.len() <= 4);
        let Some(roots) = &self.projects else {
            if self.last_good.iter().any(|root| path.starts_with(root)) {
                return true;
            }
            if self.engine_roots.iter().any(|root| path.starts_with(root)) {
                return false;
            }
            return homes.iter().any(|home| path.starts_with(home)) || !self.installed(path);
        };
        roots.iter().any(|root| path.starts_with(root))
    }

    /// Whether a path lies under an install root, in either of its macOS names
    /// ([`FindingHooks::data_twin`]), or in the app's folder: [`INSTALL_ROOTS`] on Unix and
    /// [`windows_install_roots`] on Windows. Below a Homebrew prefix only the folders Homebrew
    /// fills count ([`homebrew_filled`]). Input: the path. Output: the answer.
    pub(super) fn installed(&self, path: &Path) -> bool {
        let roots: Vec<PathBuf> = if cfg!(windows) {
            windows_install_roots(&self.seed.hooks)
        } else {
            INSTALL_ROOTS.iter().map(PathBuf::from).collect()
        };
        let under = |root: &Path| {
            path.starts_with(root)
                || (self.seed.hooks.data_twin)(root).is_some_and(|twin| path.starts_with(twin))
        };
        (roots.iter().any(|root| under(root)) && homebrew_filled(&self.seed.hooks, path))
            || self
                .app_folder
                .as_deref()
                .is_some_and(|folder| path.starts_with(folder))
    }
}

/// Keeps one data folder's read of the set-up projects as its last good read, or takes that
/// read back when this one failed, so a folder the engine named stays one agents can write
/// while engine.db cannot be read, after the engine restarts too. A good read that differs
/// from the kept one goes to the engine's store ([`FindingHooks::last_good_store`]) and
/// replaces the folder's entry; a failed read with no entry takes what the store holds
/// ([`FindingHooks::last_good_load`]). Past [`LAST_GOOD_ROOTS_MAX`] folders a new one is not
/// kept in memory. Inputs: the hooks, the data folder and this read, none when it failed.
/// Output: the read, and the last good folders when it failed.
fn last_good_read(
    hooks: &FindingHooks,
    data_root: &Path,
    read: Option<Vec<PathBuf>>,
) -> (Option<Vec<PathBuf>>, Vec<PathBuf>) {
    let mut kept = LAST_GOOD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(kept.len() <= LAST_GOOD_ROOTS_MAX);
    let Some(roots) = read else {
        if let Some(last) = kept.get(data_root) {
            return (None, last.clone());
        }
        let stored = (hooks.last_good_load)(data_root).unwrap_or_default();
        assert!(stored.len() <= PROJECT_ROOTS_MAX);
        assert!(stored.iter().all(|root| root.is_absolute()));
        if !stored.is_empty() && kept.len() < LAST_GOOD_ROOTS_MAX {
            kept.insert(data_root.to_path_buf(), stored.clone());
        }
        return (None, stored);
    };
    assert!(roots.len() <= PROJECT_ROOTS_MAX);
    if kept.get(data_root) != Some(&roots) {
        (hooks.last_good_store)(data_root, &roots);
    }
    if kept.contains_key(data_root) || kept.len() < LAST_GOOD_ROOTS_MAX {
        kept.insert(data_root.to_path_buf(), roots.clone());
    }
    assert!(kept.len() <= LAST_GOOD_ROOTS_MAX);
    (Some(roots), Vec::new())
}
