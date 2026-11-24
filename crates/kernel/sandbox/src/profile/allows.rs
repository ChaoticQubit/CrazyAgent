use super::*;

impl ProjectSandbox {
    /// Adds one extra read-only root, such as a harness install directory. Input: a
    /// canonical existing folder that neither holds nor sits inside a path from
    /// [`Self::secret_paths`]. Output: the widened sandbox, or a refusal.
    pub fn allow_read(mut self, root: &Path) -> Result<Self, SandboxError> {
        if self.read_roots.len() >= READ_ROOTS_MAX {
            return Err(invalid("too many read roots"));
        }
        granted_root(
            root,
            &owner_home()?,
            &self.secret_paths(),
            &(self.hooks.access.harness_folders)(),
        )?;
        self.read_roots.push(root.to_path_buf());
        Ok(self)
    }

    /// Lets a Node script harness find the `node` beside it, as its sign-in does: under an npm
    /// prefix ([`crate::harnesses::node_prefix`]) only `bin`, the harness's package folder and
    /// the real file `bin/node` links to become readable, Seatbelt denying credential names
    /// below each folder, and `bin` goes first on PATH; a prefix or node in the project or
    /// agent-writable is skipped. Input: the canonical program. Output: the sandbox, or a refusal.
    pub fn allow_node_for(mut self, program: &Path) -> Result<Self, SandboxError> {
        let Some(prefix) = (self.hooks.node_prefix)(program) else {
            return Ok(self);
        };
        let Some(package) = node_package(&prefix, program) else {
            return Ok(self);
        };
        let folders = [canonical(&prefix.join("bin"))?, canonical(&package)?];
        let node = canonical(&prefix.join("bin").join("node"))?;
        let agents = folders.iter().chain([&node]).any(|path| {
            (self.hooks.in_crazyagent_folder)(path)
                || path.starts_with(&self.project_root)
                || self
                    .writable_roots()
                    .any(|root| path.starts_with(root) || root.starts_with(path))
        });
        if agents {
            return Ok(self);
        }
        for folder in folders {
            if !self.reads(&folder) {
                self = self.allow_node_folder(&folder)?;
            }
        }
        if !self.reads(&node) {
            self = self.allow_node_file(&node)?;
        }
        self.node_bin = Some(prefix.join("bin"));
        assert!(self.node_bin.as_deref().is_some_and(Path::is_absolute));
        Ok(self)
    }

    /// Lets a script harness's interpreters run as the tool resolver found them
    /// ([`kernel_processes::finding::Resolved::grants`]), such as the `node` an npm-built Codex asks `env`
    /// for from nvm, volta or Homebrew: each interpreter's folder becomes readable as a
    /// toolchain folder unless a system or granted folder covers it. Input: the resolved
    /// harness. Output: the sandbox, or a refusal for an interpreter in the project or in a
    /// folder the agent can write.
    pub fn allow_interpreters(
        mut self,
        resolved: &kernel_processes::finding::Resolved,
    ) -> Result<Self, SandboxError> {
        for file in resolved.grants().into_iter().skip(1) {
            let folder = file
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| invalid("an interpreter must sit in a folder"))?;
            let agents = (self.hooks.in_crazyagent_folder)(&folder)
                || folder.starts_with(&self.project_root)
                || self
                    .writable_roots()
                    .any(|root| folder.starts_with(root) || root.starts_with(&folder));
            if agents {
                return Err(invalid("an interpreter sits where the agent can write"));
            }
            if !self.reads(&folder) {
                self = self.allow_node_folder(&folder)?;
            }
        }
        Ok(self)
    }

    /// Whether a system, granted or toolchain folder already makes a path readable. Only
    /// Seatbelt grants the toolchain folders, so elsewhere they never count. Input: an
    /// absolute path. Output: the answer.
    fn reads(&self, path: &Path) -> bool {
        let tools: &[PathBuf] = if cfg!(target_os = "macos") {
            &self.tool_roots
        } else {
            &[]
        };
        SYSTEM_READ_ROOTS
            .iter()
            .map(Path::new)
            .chain(self.read_roots.iter().map(PathBuf::as_path))
            .chain(tools.iter().map(PathBuf::as_path))
            .any(|root| path.starts_with(root))
    }

    /// Grants the real `node` an npm prefix links to, such as the engine's own Node in its
    /// setup folder, read-only and runnable: under Seatbelt as one toolchain file, elsewhere
    /// its folder as a read root. Input: the canonical existing file. Output: the widened
    /// sandbox, or a refusal when it fails the toolchain file or read root checks.
    fn allow_node_file(mut self, node: &Path) -> Result<Self, SandboxError> {
        if !cfg!(target_os = "macos") {
            let folder = node
                .parent()
                .ok_or_else(|| invalid("node must sit in a folder"))?;
            return self.allow_read(folder);
        }
        if self.tool_files.iter().any(|file| file == node) {
            return Ok(self);
        }
        if self.tool_files.len() >= TOOL_FILES_MAX {
            return Err(invalid("too many toolchain files"));
        }
        clear_of_secrets(node, &self.secret_paths())?;
        if !clean_absolute(node) || !node.is_file() || canonical(node)? != *node {
            return Err(invalid("toolchain files must be existing canonical files"));
        }
        self.tool_files.push(node.to_path_buf());
        assert!(self.tool_files.len() <= TOOL_FILES_MAX);
        Ok(self)
    }

    /// Grants one folder of a Node script harness read-only: under Seatbelt as a toolchain
    /// folder, so every credential name below it stays denied, elsewhere as a read root.
    /// Input: a canonical existing folder. Output: the widened sandbox, or a refusal when the
    /// folder fails [`Self::allow_read`]'s checks or the toolchain folders are full.
    fn allow_node_folder(mut self, folder: &Path) -> Result<Self, SandboxError> {
        if !cfg!(target_os = "macos") {
            return self.allow_read(folder);
        }
        if self.tool_roots.len() >= TOOL_ROOTS_MAX {
            return Err(invalid("too many toolchain folders"));
        }
        granted_root(
            folder,
            &owner_home()?,
            &self.secret_paths(),
            &(self.hooks.access.harness_folders)(),
        )?;
        self.tool_roots.push(folder.to_path_buf());
        assert!(self.tool_roots.len() <= TOOL_ROOTS_MAX);
        Ok(self)
    }

    /// Lets the agent follow one folder link that leads to a read root it already has, such
    /// as the version link uv keeps beside a managed Python: only the link's own text becomes
    /// readable, and no deny gives way to it, since the profile writes it before every deny.
    /// Input: a clean absolute link in a real folder, outside the hidden, kept-out and
    /// [`Self::secret_paths`], leading to a read root. Output: the sandbox, or `Invalid`.
    pub fn allow_read_link(mut self, link: &Path) -> Result<Self, SandboxError> {
        if self.links.len() >= LINKS_MAX {
            return Err(invalid("too many links to read roots"));
        }
        clear_of_secrets(link, &self.secret_paths())?;
        let is_link =
            std::fs::symlink_metadata(link).is_ok_and(|metadata| metadata.file_type().is_symlink());
        let in_real_folder = link
            .parent()
            .is_some_and(|parent| canonical(parent).is_ok_and(|real| real == parent));
        if !clean_absolute(link) || !is_link || !in_real_folder {
            return Err(invalid(
                "a link to a read root must be a clean absolute link in a real folder",
            ));
        }
        let denied = self
            .hidden
            .iter()
            .chain(self.kept_out.iter())
            .any(|path| link.starts_with(path));
        if denied {
            return Err(invalid(
                "a link to a read root must lie outside every hidden and kept-out path",
            ));
        }
        let target = canonical(link)?;
        assert!(target.is_absolute());
        if !self.read_roots.contains(&target) {
            return Err(invalid("a link must lead to a read root already granted"));
        }
        self.links.push(link.to_path_buf());
        assert!(self.links.len() <= LINKS_MAX);
        Ok(self)
    }

    /// Lets the agent start one exact program the engine itself launches, such as its
    /// harness or the engine executable, from an app bundle's `Contents/MacOS`, which no
    /// agent may start otherwise (OP-24). One inside a folder the agent can write stays
    /// denied, since the agent could replace it. Input: a canonical existing regular file
    /// clear of [`Self::secret_paths`]. Output: the widened sandbox, or `Invalid`.
    pub fn allow_exec(mut self, program: &Path) -> Result<Self, SandboxError> {
        clear_of_secrets(program, &self.secret_paths())?;
        if !clean_absolute(program) || !program.is_file() || canonical(program)? != *program {
            return Err(invalid(
                "an allowed program must be an existing canonical file",
            ));
        }
        if self.execs.iter().any(|exec| exec == program) {
            return Ok(self);
        }
        if self.execs.len() >= EXECS_MAX {
            return Err(invalid("too many allowed programs"));
        }
        self.execs.push(program.to_path_buf());
        Ok(self)
    }

    /// Lets the agent run the engine executable that serves its team tools, relay hooks and
    /// monitor: the engine's folder read-only and, by [`Self::allow_exec`], the file itself
    /// startable by exact path, even from `CrazyAgent.app/Contents/MacOS` where the macOS
    /// service runs it (OP24R2-1). Input: the engine's canonical path. Output: the widened
    /// sandbox, or `Invalid`.
    pub fn allow_engine(self, engine: &Path) -> Result<Self, SandboxError> {
        let folder = engine
            .parent()
            .ok_or_else(|| invalid("the engine must sit in a folder"))?;
        self.allow_read(folder)?.allow_exec(engine)
    }

    /// Grants a project's toolchains read-only (see [`crate::toolchains::Toolchains::detect`]):
    /// each folder as [`Self::allow_read`] checks it, with every credential name below it
    /// denied, and each file when it is a canonical regular file clear of
    /// [`Self::secret_paths`]. Only Seatbelt grants them; the Linux and Windows boundaries
    /// leave them out. Input: the toolchains. Output: the widened sandbox, or `Invalid`.
    pub fn allow_tools(
        mut self,
        tools: &crate::toolchains::Toolchains,
    ) -> Result<Self, SandboxError> {
        let owner = owner_home()?;
        let secrets = self.secret_paths();
        for root in tools.roots() {
            if self.tool_roots.contains(root) {
                continue;
            }
            if self.tool_roots.len() >= TOOL_ROOTS_MAX {
                return Err(invalid("too many toolchain folders"));
            }
            granted_root(
                root,
                &owner,
                &secrets,
                &(self.hooks.access.harness_folders)(),
            )?;
            self.tool_roots.push(root.clone());
        }
        for file in tools.files() {
            if self.tool_files.contains(file) {
                continue;
            }
            if self.tool_files.len() >= TOOL_FILES_MAX {
                return Err(invalid("too many toolchain files"));
            }
            clear_of_secrets(file, &secrets)?;
            if !clean_absolute(file) || !file.is_file() || canonical(file)? != *file {
                return Err(invalid("toolchain files must be existing canonical files"));
            }
            self.tool_files.push(file.clone());
        }
        assert!(self.tool_roots.len() <= TOOL_ROOTS_MAX);
        assert!(self.tool_files.len() <= TOOL_FILES_MAX);
        Ok(self)
    }

    /// Grants the owner's extra read-only folders for this project (see
    /// [`crate::extra_paths::checked`]) as toolchain folders are granted, with credential names
    /// carved out, skipping each one that is gone or no longer passes
    /// [`Self::allow_read`]'s checks. Input: the saved folders. Output: the widened sandbox
    /// and how many folders were skipped.
    pub fn allow_extra_reads(mut self, paths: &[PathBuf]) -> Result<(Self, usize), SandboxError> {
        let owner = owner_home()?;
        let secrets = self.secret_paths();
        let mut skipped = 0;
        for path in paths {
            if self.tool_roots.contains(path) {
                continue;
            }
            if self.tool_roots.len() >= TOOL_ROOTS_MAX
                || granted_root(
                    path,
                    &owner,
                    &secrets,
                    &(self.hooks.access.harness_folders)(),
                )
                .is_err()
            {
                skipped += 1;
                continue;
            }
            self.tool_roots.push(path.clone());
        }
        assert!(skipped <= paths.len() && self.tool_roots.len() <= TOOL_ROOTS_MAX);
        Ok((self, skipped))
    }

    /// Grants the agent's tool cache, made by `tool_cache::prepare`, as one more writable
    /// root: a non-working-tree root under the SM1-R rule, so no `.git` entry can be made,
    /// moved or written there, and the root of `pnpm_config_store_dir`, and of `UV_CACHE_DIR`
    /// when its uv markers are in place (T8). Inputs: the cache root, a canonical existing
    /// folder clear of [`Self::secret_paths`] and of every other root, and whether its uv
    /// markers are in place. Output: the widened sandbox, or `Invalid` for an unsafe or second
    /// cache.
    pub fn allow_tool_cache(mut self, root: &Path, uv: bool) -> Result<Self, SandboxError> {
        if self.tool_cache.is_some() {
            return Err(invalid("the sandbox already has a tool cache"));
        }
        granted_root(
            root,
            &owner_home()?,
            &self.secret_paths(),
            &(self.hooks.access.harness_folders)(),
        )?;
        let overlaps = self
            .writable_roots()
            .chain(self.read_roots.iter())
            .chain(self.tool_roots.iter())
            .chain([&self.project_root])
            .any(|other| other.starts_with(root) || root.starts_with(other));
        if overlaps {
            return Err(invalid("the tool cache must lie outside every other root"));
        }
        self.tool_cache = Some((root.to_path_buf(), uv));
        assert!(self.writes(root));
        self.confinement_kept()?;
        Ok(self)
    }

    /// The program `name` the agent's PATH finds first, if the engine may run it for the
    /// agent (T8 C3), as [`Self::trusted_program`] decides. Input: a plain program name.
    /// Output: the canonical path, or None.
    pub fn tool_program(&self, name: &str) -> Option<PathBuf> {
        assert!(!name.is_empty() && !name.contains('/'));
        let homes = crate::toolchains::ToolHomes::at(&self.owner_home);
        let path = crate::toolchains::agent_path(&homes, AGENT_PATH);
        let found = std::env::split_paths(&path)
            .map(|folder| folder.join(name))
            .find(|candidate| candidate.is_file())?;
        self.trusted_program(&found)
    }

    /// A found program the engine may run for the agent (T8 C3): its canonical path when
    /// that lies in a read-only system or granted tool root and in no root the agent can
    /// write, so the agent cannot swap the program the engine runs. Input: the path found.
    /// Output: the canonical path, or None.
    pub fn trusted_program(&self, found: &Path) -> Option<PathBuf> {
        let program = canonical(found).ok()?;
        let read_only = SYSTEM_READ_ROOTS
            .iter()
            .map(Path::new)
            .chain(self.tool_roots.iter().map(PathBuf::as_path))
            .any(|root| program.starts_with(root));
        let writable = self
            .writable_roots()
            .any(|root| program.starts_with(root) || root.starts_with(&program));
        (read_only && !writable && program.is_file()).then_some(program)
    }

    /// Adds the agent's short writable temp folder (see [`crate::toolchains::make_tmp`]), which
    /// becomes its `TMPDIR`. Input: a canonical existing folder clear of
    /// [`Self::secret_paths`]. Output: the widened sandbox, or `Invalid` for an unsafe or
    /// second folder.
    pub fn allow_scratch(mut self, tmp: &Path) -> Result<Self, SandboxError> {
        if self.scratch.is_some() {
            return Err(invalid("the sandbox already has a temp folder"));
        }
        granted_root(
            tmp,
            &owner_home()?,
            &self.secret_paths(),
            &(self.hooks.access.harness_folders)(),
        )?;
        self.scratch = Some(tmp.to_path_buf());
        self.confinement_kept()?;
        Ok(self)
    }

    /// Keeps one folder inside a granted root out of the agent's reach, read and write, as
    /// the secret paths are; the graph step hides the project's `.crazyagent/` from graphify.
    /// Input: a clean absolute path below a root, in a real parent folder, absent or a real
    /// folder. Output: the narrowed sandbox, or `Invalid`. Seatbelt holds it, the Linux
    /// bubblewrap backends too where no second mount shows it; plain Landlock and Windows refuse.
    pub fn hide(mut self, folder: &Path) -> Result<Self, SandboxError> {
        if self.hidden.len() >= HIDDEN_MAX {
            return Err(invalid("too many hidden folders"));
        }
        let inside = self
            .writable_roots()
            .chain(self.read_roots.iter())
            .any(|root| folder.starts_with(root) && folder != root.as_path());
        let parent = folder.parent().filter(|parent| {
            parent
                .canonicalize()
                .is_ok_and(|real| real.as_path() == *parent)
        });
        let plain = fs::symlink_metadata(folder).map_or_else(
            |error| error.kind() == io::ErrorKind::NotFound,
            |metadata| metadata.is_dir() && !metadata.file_type().is_symlink(),
        );
        if !clean_absolute(folder) || !inside || parent.is_none() || !plain {
            return Err(invalid(
                "a hidden folder must be a plain folder path inside a granted root",
            ));
        }
        self.hidden.push(folder.to_path_buf());
        Ok(self)
    }

    /// Keeps the secret files and folders of the project folder `root` closed to the agent
    /// (DEC-147.9): contents, attribute values, writes, runs, links and clones of every path
    /// there with a name in [`crate::shared_folders::SECRET_NAMES`], in any letter case, are denied,
    /// names and sizes stay visible, except under `root/.git` and inside the agent's own
    /// writable roots below it, its worktree or clone, never `root` itself. Each of `revealed`, the secrets the owner
    /// shared back, is readable again, never writable. The deny comes before every older
    /// deny in the profile, so none of those gives way. Inputs: a clean canonical folder the
    /// sandbox already reads and at most [`crate::shared_folders::SECRETS_MAX`] clean paths strictly
    /// inside it. Output: the sandbox, or `Invalid` for a second call or a path out of shape.
    pub fn keep_project_secrets(
        mut self,
        root: &Path,
        revealed: &[PathBuf],
    ) -> Result<Self, SandboxError> {
        if self.project_secrets.is_some() {
            return Err(invalid("the project's secrets are already kept closed"));
        }
        let readable = self
            .read_roots
            .iter()
            .chain([&self.project_root])
            .any(|read| read == root);
        if !clean_absolute(root) || !readable || canonical(root)? != root {
            return Err(invalid(
                "the folder whose secrets stay closed must be a canonical folder the sandbox reads",
            ));
        }
        if revealed.len() > crate::shared_folders::SECRETS_MAX
            || !revealed.iter().all(|path| {
                clean_absolute(path)
                    && path.starts_with(root)
                    && path != root
                    && !path.starts_with(root.join(".git"))
            })
        {
            return Err(invalid(
                "a shared-back secret must be a clean path inside the project folder",
            ));
        }
        self.project_secrets = Some(root.to_path_buf());
        self.secrets_revealed = revealed.to_vec();
        Ok(self)
    }

    /// Keeps every top-level entry of the [`Self::keep_project_secrets`] folder closed as
    /// [`Self::keep_out`] does but `open`, so one made after launch stays closed (L4 r1 L1);
    /// own writable roots below it stay open, never the folder itself. Input: at most
    /// [`OPEN_TOPS_MAX`] names, each [`plain_top`]. Output: the narrowed sandbox, or `Invalid`
    /// without that folder, for a second call or for a name out of shape.
    pub fn open_only_tops(mut self, open: &[String]) -> Result<Self, SandboxError> {
        if self.project_secrets.is_none() || self.open_tops.is_some() {
            return Err(invalid(
                "the open top-level names need the project folder, once",
            ));
        }
        if open.len() > OPEN_TOPS_MAX || !open.iter().all(|name| plain_top(name)) {
            return Err(invalid("an open top-level name must be one plain name"));
        }
        self.open_tops = Some(open.to_vec());
        Ok(self)
    }

    /// Lets the agent write one shared top-level folder of the project folder that git does
    /// not track (DEC-147.9 Read-write) as one more writable root, pinned in place; the secret
    /// names inside it stay closed, and every older deny still wins. Never a hidden folder, an
    /// engine folder or one named `context` ([`crate::shared_folders::never_written`]). Input: a canonical existing folder
    /// directly inside the folder [`Self::keep_project_secrets`] named, clear of
    /// [`Self::secret_paths`] and of every other writable root. Output: the widened sandbox, or
    /// `Invalid`; only Seatbelt holds it.
    pub fn allow_shared_write(mut self, folder: &Path) -> Result<Self, SandboxError> {
        if self.shared_writes.len() >= crate::shared_folders::SHARED_WRITES_MAX {
            return Err(invalid("too many shared writable folders"));
        }
        let in_project = self
            .project_secrets
            .as_ref()
            .is_some_and(|root| folder.parent() == Some(root.as_path()));
        let kept_read_only = match (self.project_secrets.as_ref(), folder.file_name()) {
            (Some(root), Some(name)) => name.to_str().is_none_or(|name| {
                crate::shared_folders::never_written(self.hooks.access, root, name)
            }),
            _ => true,
        };
        if !in_project || kept_read_only {
            return Err(invalid(
                "a shared writable folder must be a top-level project folder kept under its secret rule",
            ));
        }
        granted_root(
            folder,
            &owner_home()?,
            &self.secret_paths(),
            &(self.hooks.access.harness_folders)(),
        )?;
        if self
            .writable_roots()
            .any(|root| root.starts_with(folder) || folder.starts_with(root))
        {
            return Err(invalid(
                "a shared writable folder must lie outside every other writable root",
            ));
        }
        self.shared_writes.push(folder.to_path_buf());
        assert!(self.writes(folder));
        assert!(self.shared_writes.len() <= crate::shared_folders::SHARED_WRITES_MAX);
        self.confinement_kept()?;
        Ok(self)
    }

    /// The Seatbelt rule of [`Self::open_only_tops`]: every path below the project folder is
    /// kept out but the open top-level names and the agent's own writable roots there.
    /// Output: the rule, empty when no names were set.
    pub(super) fn closed_tops_rule(&self) -> String {
        let (Some(root), Some(open)) = (self.project_secrets.as_ref(), self.open_tops.as_ref())
        else {
            return String::new();
        };
        assert!(clean_absolute(root));
        let pattern = seatbelt_pattern(root);
        let mut text = format!("{KEPT_OUT_DENY}\n  (require-all\n    (regex #\"^{pattern}/\")");
        let names: Vec<String> = open.iter().map(|name| regex_escape(name)).collect();
        for suffix in ["$", "/"] {
            for body in regex_bodies(&format!("^{pattern}/"), &names, suffix) {
                text.push_str(&format!("\n    (require-not (regex #\"{body}\"))"));
            }
        }
        for own in self.writable_roots().filter(|own| {
            own.starts_with(root) && own.as_path() != root.as_path() && clean_absolute(own)
        }) {
            text.push_str(&format!(
                "\n    (require-not (subpath \"{}\"))",
                own.display()
            ));
        }
        text.push_str("))\n");
        text
    }

    /// The Seatbelt rules of [`Self::keep_project_secrets`]: the deny of secret names in the
    /// project folder outside `.git` and the agent's own writable roots there (never the
    /// shared writable folders); no clone of any folder, and no move or removal of a folder
    /// inside a shared writable folder, so no folder that holds a secret leaves the name rule
    /// by a plain-named parent (L4 r1 H2); then the read allow of env templates
    /// ([`crate::shared_folders::SECRET_TEMPLATES`]) and of each shared-back secret. Output: the
    /// rules, empty when no folder was named.
    pub(super) fn project_secret_rules(&self) -> String {
        let Some(root) = self
            .project_secrets
            .as_ref()
            .filter(|root| clean_absolute(root))
        else {
            return String::new();
        };
        let names: Vec<String> = crate::shared_folders::SECRET_NAMES
            .iter()
            .map(|name| fold_case(name))
            .collect();
        let own: String = self
            .writable_roots()
            .filter(|own| {
                own.starts_with(root)
                    && own.as_path() != root.as_path()
                    && !self.shared_writes.contains(own)
                    && clean_absolute(own)
            })
            .map(|own| format!("\n    (require-not (subpath \"{}\"))", own.display()))
            .collect();
        let prefix = format!("^{}/(.*/)?", seatbelt_pattern(root));
        let mut text = String::new();
        for body in regex_bodies(&prefix, &names, "(/|$)") {
            text.push_str(&format!(
                "{KEPT_OUT_DENY}\n  (require-all\n    (regex #\"{body}\")\n    (require-not (subpath \"{}/.git\")){own}))\n",
                root.display()
            ));
        }
        text.push_str("(deny file-clone (vnode-type DIRECTORY))\n");
        for folder in self
            .shared_writes
            .iter()
            .filter(|folder| clean_absolute(folder))
        {
            text.push_str(&format!(
                "(deny file-write-unlink\n  (require-all\n    (subpath \"{}\")\n    (vnode-type DIRECTORY)))\n",
                folder.display()
            ));
        }
        text.push_str(&format!(
            "(allow file-read-data file-read-xattr\n  (regex #\"^{}/(.*/)?{}$\"))\n",
            seatbelt_pattern(root),
            fold_case(crate::shared_folders::SECRET_TEMPLATES)
        ));
        let revealed: Vec<&PathBuf> = self
            .secrets_revealed
            .iter()
            .filter(|path| clean_absolute(path))
            .collect();
        if !revealed.is_empty() {
            text.push_str("(allow file-read-data file-read-xattr");
            for path in revealed {
                text.push_str(&format!("\n  (subpath \"{}\")", path.display()));
            }
            text.push_str(")\n");
        }
        text
    }

    /// The project folder whose secret names this sandbox keeps closed, if any.
    pub fn project_secrets(&self) -> Option<&Path> {
        self.project_secrets.as_deref()
    }

    /// The shared top-level project folders this sandbox lets the agent write.
    pub fn shared_writes(&self) -> &[PathBuf] {
        &self.shared_writes
    }

    /// Holds the project's agent folders choice in this sandbox (DEC-124): a Sandboxed IDE
    /// terminal keeps to what the owner chose for the IDE. Only Seatbelt holds it, so the
    /// Linux and Windows boundaries refuse a sandbox with a choice other than Allow all
    /// changes. Input: the rule. Output: the sandbox.
    pub fn protect_agent_files(mut self, rule: crate::policy::AgentFilesRule) -> Self {
        self.agent_files = Some(rule);
        self
    }

    /// Marks the home as one the owner keeps for their own tools, which no launch resets, as
    /// the IDE terminal's and the IDE server's are: the profile leaves out
    /// [`agent_config_denies`], so `npm login`, `git config --global` and `cargo install`
    /// write there and copies keep their flags (RV-DS241-188, -187), and [`Self::install`]
    /// makes no folder there, as [`owner_terminal_env`] leaves out [`AGENT_HOME_ENV`].
    /// Output: the sandbox.
    pub fn keep_owner_config(mut self) -> Self {
        self.owner_config = true;
        self
    }

    /// Names the engine's data root the agent home is judged by, as a master turn's home may
    /// be a configured RCLI home the owner chose in one of their own folders (never HOME
    /// itself or a folder holding it, which [`Self::new`] refuses): [`Self::install`] makes
    /// and removes nothing in a home not strictly below the root, so a link a dotfile manager
    /// made there stays, and clears what holds a config folder's name in one that is
    /// (rv-code2 b8 F1). The profile keeps every deny either way. Input: the canonical data
    /// root. Output: the sandbox.
    #[cfg(unix)]
    pub fn anchor_home(mut self, engine_root: &Path) -> Self {
        assert!(engine_root.is_absolute());
        let below = self.agent_home != engine_root && self.agent_home.starts_with(engine_root);
        self.engine_home = Some(below);
        self
    }

    /// Keeps one file or folder inside a granted root from being read, written or run: its
    /// name, size and dates stay visible, its contents, folder listing and attribute values
    /// do not, so a tool that stats an import does not fail; the graph step keeps the
    /// project's secret-like and git-ignored paths from graphify (MAP-2). Input: a clean
    /// absolute path below a root, of any kind or absent. Output: the narrowed sandbox, or
    /// `Invalid` past [`KEPT_OUT_MAX`]. Only Seatbelt holds it, so the Linux and Windows
    /// boundaries refuse such a sandbox.
    pub fn keep_out(mut self, path: &Path) -> Result<Self, SandboxError> {
        if self.kept_out.len() >= KEPT_OUT_MAX {
            return Err(invalid("too many paths kept out"));
        }
        let inside = self
            .writable_roots()
            .chain(self.read_roots.iter())
            .any(|root| path.starts_with(root) && path != root.as_path());
        if !clean_absolute(path) || !inside {
            return Err(invalid(
                "a path kept out must be a clean absolute path inside a granted root",
            ));
        }
        self.kept_out.push(path.to_path_buf());
        Ok(self)
    }

    /// Whether [`Self::keep_out`] takes the form of `path`, a clean absolute path, so a
    /// caller can check every path before it makes anything. Input: the path. Output: the
    /// answer; a path kept out must still lie inside a granted root.
    pub fn keepable(path: &Path) -> bool {
        clean_absolute(path)
    }

    /// Lets a native-mode lead run its own tmux server (DEC-104): an exact socket grant, as
    /// `allow_unix_socket` gives, for its own socket in its short temp folder, and the tmux
    /// program itself, readable and startable as one file, never its folder: the bundled one
    /// sits in the app's `Contents/MacOS` beside the engine (DEC-157). No other Unix
    /// socket becomes reachable, the engine's own tmux server included, and the server and
    /// every engine client on it stay inside this profile; its teammates run in a server of
    /// their own (see [`Self::serve_tmux_server`]). Only Seatbelt holds it. Input: the socket
    /// (`terminal::harness_socket`) and the canonical tmux program. Output: the widened
    /// sandbox, `Invalid`, or `Unavailable` off macOS.
    pub fn allow_tmux_server(self, socket: &Path, tmux: &Path) -> Result<Self, SandboxError> {
        self.tmux_server(socket, tmux, false)
    }

    /// Lets a native lead's teammates run their own tmux server (rv-audit S1 F1): as
    /// [`Self::allow_tmux_server`], but the socket is served only, as
    /// [`Self::serve_unix_socket`] gives, so no teammate pane the server forks connects to
    /// it, unlinks it or binds a server of its own there. Input: the socket
    /// (`terminal::teammates_socket`) and the canonical tmux program. Output: the widened
    /// sandbox, `Invalid`, or `Unavailable` off macOS.
    pub fn serve_tmux_server(self, socket: &Path, tmux: &Path) -> Result<Self, SandboxError> {
        self.tmux_server(socket, tmux, true)
    }

    /// Splits a native-mode lead's sandbox into the lead's and its teammates' (DEC-104,
    /// rv-audit S1 F1, TTDL F1). The lead runs its own tmux server at `own` and only reaches
    /// the engine's caller socket at `callers` and its teammates' server at `mates`. The
    /// teammates serve `mates`, only reach `callers` and are shut out of `own`, so no agent
    /// binds, blocks or replaces the caller socket the engine answers on (DEC-111 Q18,
    /// rv-secreview RH-1) and no teammate holds a listener or places a node where the lead
    /// connects. They read but never write the launch folders the engine installs in the
    /// lead's home, its hooks, skills and relay plugin, which the lead's Claude Code runs
    /// (TPH), nor move, replace or re-permission that home itself (TPH r1 F1), nor the
    /// shell snapshot and session scripts its Bash commands run first (TPH-2). The teammates
    /// run in their own home and only read the lead's, except the team folders Claude Code
    /// shares, where they make no link or folder but a lock, and never write the `git` on the
    /// lead's path (TPH-2 C). They neither read nor write the lead's own temp folder in the
    /// short temp folder they share ([`crate::toolchains::native_lead_tmp`], TPH-2 residual), so no
    /// teammate reads, races or plants a file the lead stages there. Both are shut out of the
    /// name the engine binds the caller socket at before it moves it over `callers`, so
    /// neither swaps its own listener in there (rv-secreview RH2R2-1). The lead's part of the
    /// relay mailbox is hidden from the teammates but for its `relay.json`, which they only
    /// read, and they never move or re-permission the mailbox or the temp folder holding it
    /// (relay mailbox forgery C1). Neither the lead nor a teammate moves the tool mailbox, so
    /// no answer is written outside the rule that keeps answers the engine's. Inputs: the
    /// three sockets, the canonical tmux program and
    /// the teammates' home beside the lead's, its team folders already made, with the
    /// mailbox in the agent temp folder. Output: the lead's and the teammates' sandboxes,
    /// `Invalid`, or `Unavailable` off macOS.
    #[cfg(unix)]
    pub fn native_team(
        self,
        own: &Path,
        mates: &Path,
        callers: &Path,
        tmux: &Path,
        mates_home: &Path,
    ) -> Result<(Self, Self), SandboxError> {
        let home = self.agent_home.clone();
        granted_root(
            mates_home,
            &self.owner_home,
            &self.secret_paths(),
            &(self.hooks.access.harness_folders)(),
        )?;
        if mates_home.starts_with(&home) || home.starts_with(mates_home) {
            return Err(invalid("the teammates' home must sit beside the lead's"));
        }
        let shared: Vec<PathBuf> = CLAUDE_TEAM_SHARED
            .iter()
            .map(|folder| home.join(folder))
            .collect();
        for folder in &shared {
            granted_root(
                folder,
                &self.owner_home,
                &self.secret_paths(),
                &(self.hooks.access.harness_folders)(),
            )?;
        }
        let binding = (self.hooks.binding_path)(callers);
        let mailbox = self.agent_tmp.join(self.hooks.mailbox_folder);
        let lead_part = mailbox.join(self.hooks.relay_lead_part);
        let mut teammates = self
            .clone()
            .serve_tmux_server(mates, tmux)?
            .reach_unix_socket(callers)?
            .shut_unix_socket(own)?
            .shut_unix_socket(&binding)?
            .allow_read(&home)?
            .hide(&lead_part)?;
        let lead_tmp = teammates
            .scratch
            .as_deref()
            .map(crate::toolchains::native_lead_tmp)
            .ok_or_else(|| invalid("a native team needs the agent temp folder"))?;
        teammates = teammates.hide(&lead_tmp)?;
        teammates.agent_home = mates_home.to_path_buf();
        teammates.revealed = vec![lead_part.join(self.hooks.relay_roster_file)];
        teammates.read_only = vec![
            home.join(self.hooks.launch_state),
            home.join(self.hooks.relay_plugin_folder),
        ];
        teammates.read_only.extend(
            self.hooks
                .claude_bash_preludes
                .iter()
                .map(|folder| home.join(folder)),
        );
        teammates.read_only.extend(
            self.scratch
                .iter()
                .map(|scratch| scratch.join(crate::toolchains::GIT_FOLDER)),
        );
        let answers = mailbox.clone();
        teammates.answers = Some(answers.clone());
        teammates.fixed = vec![home.clone(), self.agent_tmp.clone(), mailbox];
        teammates.fixed.extend(shared.iter().cloned());
        teammates.shared = shared;
        teammates.teammates = true;
        if teammates.writes(&home) {
            return Err(invalid("a teammates' writable root holds the lead's home"));
        }
        let mut lead = self
            .allow_tmux_server(own, tmux)?
            .reach_unix_socket(callers)?
            .reach_unix_socket(mates)?
            .shut_unix_socket(&binding)?;
        lead.fixed.push(answers.clone());
        lead.answers = Some(answers);
        lead.confinement_kept()?;
        teammates.confinement_kept()?;
        Ok((lead, teammates))
    }

    /// The writes no agent of a native team may make to the engine's answers in the tool
    /// mailbox the team shares, a reply or the part file it is written under, in any spelling
    /// the volume folds together. An agent may still remove a reply, as its tool server does
    /// once it has read one. Output: the rule text, empty for any other profile.
    pub(super) fn answers_denies(&self) -> String {
        let Some(mailbox) = &self.answers else {
            return String::new();
        };
        let rule = format!(
            "(regex #\"^{}/[^/]+({}|{})$\")",
            seatbelt_pattern(mailbox),
            any_case(self.hooks.reply_suffix),
            any_case(self.hooks.answer_part_suffix),
        );
        format!("(deny file-write* {rule})\n(allow file-write-unlink {rule})\n")
    }

    /// The writes a native lead's teammates may not make in the lead's worktree (OP-9,
    /// OP-9b), none for any other profile: in the lead's own clone, those
    /// [`teammates_clone_denies`] names; in a linked worktree, anything but the teammates'
    /// folder, and the lead's worktree record and the object store, since the engine
    /// commits the teammates' turns, nor the teammates' folder entry, nor make, move or
    /// remove anything directly in it, such as a teammate's folder, where the engine's
    /// setup git adds each teammate worktree (OP-9 r2, OP9B-C1). Output: the rule text,
    /// empty when none applies.
    pub(super) fn teammates_denies(&self) -> String {
        if !self.teammates {
            return String::new();
        }
        match (&self.clone, &self.checkout) {
            (Some(clone), _) => {
                teammates_clone_denies(self.hooks, clone, self.lead_branch.as_deref())
            }
            (None, Some(checkout)) => {
                let worktree = checkout.worktree.display();
                let root = seatbelt_pattern(&checkout.worktree);
                let teammates = any_case(self.hooks.teammates_folder);
                let text = format!(
                    "(deny file-write*\n  (require-all\n    (subpath \"{worktree}\")\n    (require-not (subpath \"{worktree}/{}\"))))\n(deny file-write*\n  (subpath \"{}\")\n  (subpath \"{}\")\n  (regex #\"^{root}/{teammates}$\"))\n(deny file-write-create file-write-unlink\n  (regex #\"^{root}/{teammates}/[^/]+$\"))\n",
                    self.hooks.teammates_folder,
                    checkout.admin.display(),
                    checkout.objects.display()
                );
                assert_eq!(text.matches("(subpath \"/").count(), 4);
                text
            }
            (None, None) => String::new(),
        }
    }

    /// Grants one tmux server in the agent temp folder, served and reached or served only.
    /// Takes the socket, the canonical tmux program and whether the socket is served only.
    /// Returns the widened sandbox, `Invalid`, or `Unavailable` off macOS.
    fn tmux_server(
        mut self,
        socket: &Path,
        tmux: &Path,
        served_only: bool,
    ) -> Result<Self, SandboxError> {
        if !cfg!(target_os = "macos") {
            return Err(SandboxError::Unavailable(TMUX_SERVER_NEEDS_SEATBELT.into()));
        }
        let scratch = self
            .scratch
            .clone()
            .ok_or_else(|| invalid("a tmux server needs the agent temp folder"))?;
        if self
            .sockets
            .iter()
            .chain(&self.served)
            .any(|known| known.parent() == Some(scratch.as_path()))
        {
            return Err(invalid("the sandbox already has a tmux server"));
        }
        if !clean_absolute(socket)
            || socket.parent() != Some(scratch.as_path())
            || socket.as_os_str().len() > SOCKET_PATH_BYTES_MAX
        {
            return Err(invalid("the tmux socket must sit in the agent temp folder"));
        }
        clear_of_secrets(socket, &self.secret_paths())?;
        if !clean_absolute(tmux) || !tmux.is_file() || canonical(tmux)? != tmux {
            return Err(invalid("the tmux program must be a canonical file"));
        }
        if !self.tool_files.iter().any(|file| file == tmux) {
            if self.tool_files.len() >= TOOL_FILES_MAX {
                return Err(invalid("too many toolchain files"));
            }
            self.tool_files.push(tmux.to_path_buf());
        }
        self = self.allow_exec(tmux)?;
        assert!(self.tool_files.iter().any(|file| file == tmux));
        self = if served_only {
            self.serve_unix_socket(socket)?
        } else {
            self.allow_unix_socket(socket)?
        };
        assert!(self.writes(socket.parent().unwrap_or(socket)));
        Ok(self)
    }

    /// Marks the project folder as one only the engine's own process works in under this sandbox:
    /// a folder it made for one job, a plugin server's check, a context-mode install, or a project
    /// owner git runs in, which must write `.git` and which plain owner git writes in full anyway.
    /// On Windows it gets no project guards, its top writable (DEC-193); Seatbelt and Landlock are
    /// unchanged. Input: a sandbox confined to no worktree, clone or setup. Output: it, marked.
    pub fn as_job_folder(mut self) -> Self {
        assert_eq!(self.confinements(), 0);
        assert!(!self.job_folder);
        self.job_folder = true;
        self
    }

    /// Whether the project folder holds the project guards of [`Self::protected_paths`]:
    /// true but for a folder marked by [`Self::as_job_folder`]. Output: the answer.
    pub fn holds_project_guards(&self) -> bool {
        !self.job_folder
    }

    /// The project folder this sandbox confines writes to.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    /// The agent's managed home directory.
    pub fn agent_home(&self) -> &Path {
        &self.agent_home
    }

    /// The agent's managed temp directory.
    pub fn agent_tmp(&self) -> &Path {
        &self.agent_tmp
    }

    /// The extra read-only roots, such as harness install directories.
    pub fn read_roots(&self) -> &[PathBuf] {
        &self.read_roots
    }

    /// The network reach this sandbox grants the agent.
    pub fn network(&self) -> &SandboxNetwork {
        &self.network
    }

    /// Sets how this agent's sockets are held on Linux when its network is limited. Input:
    /// the owner's setting. Output: the sandbox.
    pub fn limit_sockets(mut self, sockets: LinuxSockets) -> Self {
        self.linux_sockets = sockets;
        self
    }

    /// How this agent's sockets are held on Linux when its network is limited.
    pub fn linux_sockets(&self) -> LinuxSockets {
        self.linux_sockets
    }

    /// The extra worktrees, beside the project folder, the agent writes in and may start in.
    #[cfg(any(target_os = "linux", windows))]
    pub(crate) fn worktrees(&self) -> &[PathBuf] {
        &self.worktrees
    }

    /// The paths the agent can neither read nor list: secrets inside the project and the
    /// folders [`Self::hide`] took.
    pub(crate) fn hidden(&self) -> &[PathBuf] {
        &self.hidden
    }

    /// The project folders [`Self::keep_out`] keeps the agent out of.
    #[cfg(any(target_os = "linux", windows))]
    pub(crate) fn kept_out(&self) -> &[PathBuf] {
        &self.kept_out
    }

    /// The top-level project folders the agent alone may open, when [`Self::open_only_tops`]
    /// set them. Output: the names, or none when every top is open.
    #[cfg(any(target_os = "linux", windows))]
    pub(crate) fn open_tops(&self) -> Option<&[String]> {
        self.open_tops.as_deref()
    }

    /// The loopback ports [`Self::keep_ports`] keeps free of the agent's own servers.
    #[cfg(target_os = "linux")]
    pub(crate) fn kept_ports(&self) -> &[u16] {
        &self.kept_ports
    }

    /// The carved home and the real home it stands in for, when the agent's home is carved
    /// out of the owner's. Output: the two folders, or none.
    #[cfg(target_os = "linux")]
    pub(crate) fn carved_home(&self) -> Option<&(PathBuf, PathBuf)> {
        self.carved_home.as_ref()
    }

    /// The worktree [`Self::confine_to_worktree`] confines the agent to, with its admin folder
    /// and object store. Output: the checkout, or none.
    #[cfg(target_os = "linux")]
    pub(crate) fn checkout(&self) -> Option<&Checkout> {
        self.checkout.as_ref()
    }

    /// The clone [`Self::confine_to_clone`] confines the agent to. Output: its root, or none.
    #[cfg(target_os = "linux")]
    pub(crate) fn clone_root(&self) -> Option<&PathBuf> {
        self.clone.as_ref()
    }

    /// The teammates' folder and records [`Self::confine_to_teammates_setup`] grants. Output:
    /// the setup, or none.
    #[cfg(target_os = "linux")]
    pub(crate) fn teammates_setup(&self) -> Option<&TeammatesSetup> {
        self.setup.as_ref()
    }

    /// Tells whether [`Self::adding_worktrees`] lets the engine's worktree-add step write the
    /// project's worktree folders. Output: the answer.
    #[cfg(target_os = "linux")]
    pub(crate) fn adds_worktrees(&self) -> bool {
        self.adds_worktrees
    }

    /// The project's agent folders choice this sandbox holds, if any. Output: a copy of the
    /// rule, or none.
    #[cfg(any(target_os = "linux", windows))]
    pub(crate) fn agent_files(&self) -> Option<crate::policy::AgentFilesRule> {
        self.agent_files
    }

    /// The loopback ports a Linux that nests agents carries for this one over a bridge, the
    /// agent then having a network namespace of its own: the sockets setting is `TcpOnly`
    /// and the limited network names loopback ports only, each [`BRIDGE_PORT_MIN`] or
    /// higher. Output: the ports in rising order, or none when the agent keeps the shared
    /// network.
    pub fn bridged_ports(&self) -> Option<Vec<u16>> {
        let SandboxNetwork::Only(remotes) = &self.network else {
            return None;
        };
        if self.linux_sockets != LinuxSockets::TcpOnly || remotes.is_empty() {
            return None;
        }
        let mut ports = Vec::with_capacity(remotes.len());
        for remote in remotes {
            match remote {
                SandboxRemote::Loopback(port) if *port >= BRIDGE_PORT_MIN => ports.push(*port),
                _ => return None,
            }
        }
        ports.sort_unstable();
        ports.dedup();
        assert!(!ports.is_empty() && ports.len() <= REMOTES_MAX);
        assert!(!self.resolves_over_tcp());
        Some(ports)
    }

    /// The network a Linux that nests agents gives this one of its own, leaving only
    /// through its ports on any host: the sockets setting is `TcpOnly`, the limited network
    /// names at least one port on any host and every other remote is a loopback port
    /// [`BRIDGE_PORT_MIN`] or higher, bridged as [`ProjectSandbox::bridged_ports`] does.
    /// Output: the ports, or none when the agent keeps the shared network.
    pub fn routed(&self) -> Option<Routed> {
        let SandboxNetwork::Only(remotes) = &self.network else {
            return None;
        };
        if self.linux_sockets != LinuxSockets::TcpOnly || remotes.len() > REMOTES_MAX {
            return None;
        }
        let mut any = Vec::with_capacity(remotes.len());
        let mut bridged = Vec::with_capacity(remotes.len());
        for remote in remotes {
            match remote {
                SandboxRemote::AnyHost(port) => any.push(*port),
                SandboxRemote::Loopback(port) if *port >= BRIDGE_PORT_MIN => bridged.push(*port),
                SandboxRemote::Loopback(_) => return None,
            }
        }
        if any.is_empty() {
            return None;
        }
        any.sort_unstable();
        any.dedup();
        bridged.sort_unstable();
        bridged.dedup();
        assert!(!any.contains(&0));
        assert!(any.len() + bridged.len() <= REMOTES_MAX);
        assert!(self.bridged_ports().is_none());
        assert!(self.resolves_over_tcp());
        Some(Routed { any, bridged })
    }

    /// The port that alone keeps this agent on this computer's network on a Linux that
    /// nests agents: the sockets setting is `TcpOnly` and the limited network names
    /// loopback ports only, but one lies below [`BRIDGE_PORT_MIN`]. Output: the lowest such
    /// port, or none when the agent is bridged or shares the network for another reason.
    pub fn unbridged_port(&self) -> Option<u16> {
        let SandboxNetwork::Only(remotes) = &self.network else {
            return None;
        };
        if self.linux_sockets != LinuxSockets::TcpOnly {
            return None;
        }
        let mut lowest: Option<u16> = None;
        for remote in remotes {
            let SandboxRemote::Loopback(port) = remote else {
                return None;
            };
            if *port < BRIDGE_PORT_MIN && lowest.is_none_or(|low| *port < low) {
                lowest = Some(*port);
            }
        }
        assert!(lowest.is_none() || self.bridged_ports().is_none());
        lowest
    }

    /// True when, on Linux, this agent looks host names up over TCP: the setting is
    /// `TcpOnly` and its limited network names a remote on any host. It then may reach TCP
    /// port 53 and gets `RES_OPTIONS=use-vc`; loopback-only and empty networks get neither.
    pub fn resolves_over_tcp(&self) -> bool {
        self.linux_sockets == LinuxSockets::TcpOnly
            && matches!(&self.network, SandboxNetwork::Only(remotes)
                if remotes.iter().any(|remote| matches!(remote, SandboxRemote::AnyHost(_))))
    }

    /// Why the Linux sockets setting refuses to start `program` in this sandbox:
    /// [`LINUX_LIMITED_NETWORK_OFF`] for any limited network under `Refuse`, and
    /// [`LINUX_RESOLVER_NEEDS_UDP`] under `TcpOnly` when the agent resolves names and the
    /// program, or the interpreter a script names, is not a glibc one, and
    /// [`LINUX_HARNESS_UNCHECKED`] when that interpreter cannot be checked ([`elf::lookup`]).
    /// The setting never changes silently. Input: the harness program. Output: the refusal
    /// code, or None.
    pub fn linux_refusal(&self, program: &Path) -> Option<&'static str> {
        match (&self.network, self.linux_sockets) {
            (SandboxNetwork::Open, _) => None,
            (SandboxNetwork::Only(_), LinuxSockets::Refuse) => Some(LINUX_LIMITED_NETWORK_OFF),
            _ if self.resolves_over_tcp() => match elf::lookup(program) {
                elf::Lookup::OverTcp => None,
                elf::Lookup::NotOverTcp => Some(LINUX_RESOLVER_NEEDS_UDP),
                elf::Lookup::Unjudged => Some(LINUX_HARNESS_UNCHECKED),
            },
            _ => None,
        }
    }

    /// Paths no agent may write although they sit in a writable root: both `context/`
    /// folders, the project instructions file every later agent and the master get as the
    /// owner's text (SA vid 11), the git hooks, config and its lock, `.git/modules` and, for
    /// an agent confined to a worktree, its admin folder's `modules`, where git keeps
    /// submodule repositories.
    /// Every platform boundary keeps these read-only, except that Windows guards the config
    /// but not its lock (`windows::GIT_CONFIG_LOCK`).
    pub fn protected_paths(&self) -> Vec<PathBuf> {
        let git = self.project_root.join(".git");
        let mut paths = vec![
            self.project_root.join("context"),
            self.project_root.join(".crazyagent").join("context"),
            self.project_root
                .join(".crazyagent")
                .join(self.hooks.project_file),
            git.join("hooks"),
            git.join("config"),
            git.join("config.lock"),
            git.join("modules"),
        ];
        if let Some(checkout) = &self.checkout {
            paths.push(checkout.admin.join("modules"));
        }
        paths
    }

    /// How many confinements the sandbox holds: its own worktree, its own clone or the
    /// teammates' setup, at most one. Output: the count.
    pub(crate) fn confinements(&self) -> usize {
        usize::from(self.checkout.is_some())
            + usize::from(self.clone.is_some())
            + usize::from(self.setup.is_some())
    }

    /// Every folder this sandbox lets the agent write: the project folder, or for a
    /// confined agent its worktree, that worktree's git admin folder and the git object
    /// store, or for a clone-confined agent its clone alone, then its own home and temp
    /// dirs, its short temp folder, then its extra worktrees, then for a native lead's
    /// teammates the team folders they share in the lead's home, then its tool cache, the
    /// owner's temp folder when granted and each shared writable folder.
    pub fn writable_roots(&self) -> impl Iterator<Item = &PathBuf> {
        self.project_writes().into_iter().chain(self.other_writes())
    }

    /// The writable roots in the project: the project folder, or a confinement's own roots.
    /// Output: one to three roots.
    fn project_writes(&self) -> Vec<&PathBuf> {
        assert!(self.confinements() <= 1);
        match (&self.checkout, &self.clone, &self.setup) {
            (Some(checkout), _, _) => vec![&checkout.worktree, &checkout.admin, &checkout.objects],
            (None, Some(clone), _) => vec![clone],
            (None, None, Some(setup)) => vec![&setup.folder, &setup.records],
            (None, None, None) => vec![&self.project_root],
        }
    }

    /// Refuses a confined sandbox whose writable roots outside its confinement reach the
    /// project: one that holds the project folder or lies in it outside the confinement's
    /// own roots would leave the project writable beside the confinement. Roots disjoint
    /// from the project pass, as does any sandbox that is not confined. Output: Ok, or
    /// `Invalid`.
    pub(crate) fn confinement_kept(&self) -> Result<(), SandboxError> {
        if self.confinements() == 0 {
            return Ok(());
        }
        let own = self.project_writes();
        let project = &self.project_root;
        let reaches = self.other_writes().any(|root| {
            (root.starts_with(project) || project.starts_with(root))
                && !own.iter().any(|kept| root.starts_with(kept))
        });
        if reaches {
            return Err(invalid(
                "a writable root reaches the project outside the agent's confinement",
            ));
        }
        Ok(())
    }

    /// The writable roots besides the project's: the agent's home and temp folders, its short
    /// temp folder, extra worktrees, team folders, tool cache, the owner's temp folder when
    /// granted and each shared writable folder. Output: the roots.
    fn other_writes(&self) -> impl Iterator<Item = &PathBuf> {
        [&self.agent_home, &self.agent_tmp]
            .into_iter()
            .chain(self.scratch.iter())
            .chain(self.worktrees.iter())
            .chain(self.shared.iter())
            .chain(self.tool_cache.iter().map(|(root, _)| root))
            .chain(self.owner_tmp.iter())
            .chain(self.shared_writes.iter())
    }

    /// The read-only toolchain folders, Seatbelt only, with credential names carved out.
    pub fn tool_roots(&self) -> &[PathBuf] {
        &self.tool_roots
    }

    /// Whether the agent could write `path` or move it away: true when the path is one of
    /// [`Self::writable_roots`] or inside one. Input: an absolute canonical path. Output:
    /// the answer.
    pub fn writes(&self, path: &Path) -> bool {
        self.writable_roots().any(|root| path.starts_with(root))
    }
}
