use super::*;

impl ProjectSandbox {
    /// The Seatbelt profile text for this agent. Reading another process's arguments through
    /// sysctl is refused only with both the named sysctl deny and the explicit `process-info*`
    /// deny. What stays open: every read of a process of the same sandbox, the list of process
    /// ids, which carries only ids, and for `nohup`, by its own path, the one read of a process
    /// outside that it needs to detach from the console. Output: the text.
    pub fn profile(&self) -> String {
        let mut text = String::from("(version 1)\n(deny default)\n");
        text.push_str("(allow process-exec)\n(allow process-fork)\n");
        text.push_str(&app_exec_denies(
            self.writable_roots(),
            self.execs.iter().filter(|program| !self.writes(program)),
        ));
        text.push_str("(allow signal (target same-sandbox))\n");
        text.push_str("(allow sysctl-read)\n");
        text.push_str("(deny sysctl-read (sysctl-name-prefix \"kern.procargs\"))\n");
        text.push_str("(deny process-info*)\n");
        text.push_str("(allow process-info* (target same-sandbox))\n");
        text.push_str("(allow process-info-listpids)\n");
        text.push_str("(allow process-info-pidinfo (process-path \"/usr/bin/nohup\"))\n");
        text.push_str("(allow iokit-open (iokit-registry-entry-class \"RootDomainUserClient\"))\n");
        text.push_str("(allow ipc-posix-sem)\n");
        text.push_str(
            "(allow ipc-posix-shm-read* (ipc-posix-name \"apple.shm.notification_center\"))\n",
        );
        text.push_str("(allow pseudo-tty)\n");
        text.push_str("(allow mach-lookup");
        for service in MACH_SERVICES {
            text.push_str(&format!("\n  (global-name \"{service}\")"));
        }
        text.push_str(")\n");
        text.push_str("(allow file-read* file-map-executable");
        for root in SYSTEM_READ_ROOTS {
            text.push_str(&format!("\n  (subpath \"{root}\")"));
        }
        text.push_str(")\n(allow file-read*");
        for literal in METADATA_LITERALS {
            text.push_str(&format!("\n  (literal \"{literal}\")"));
        }
        text.push_str(")\n");
        text.push_str("(allow file-read-metadata");
        for root in self
            .writable_roots()
            .chain(self.read_roots.iter())
            .chain(self.tool_roots.iter())
            .chain(self.tool_files.iter())
        {
            text.push_str(&format!("\n  (path-ancestors \"{}\")", root.display()));
        }
        text.push_str(")\n(allow file-read* file-map-executable");
        for root in self
            .writable_roots()
            .chain(self.read_roots.iter())
            .chain(self.tool_roots.iter())
        {
            text.push_str(&format!("\n  (subpath \"{}\")", root.display()));
        }
        for file in &self.tool_files {
            text.push_str(&format!("\n  (literal \"{}\")", file.display()));
        }
        text.push_str(")\n(allow file-write*");
        for root in self.writable_roots() {
            text.push_str(&format!("\n  (subpath \"{}\")", root.display()));
        }
        text.push_str(")\n");
        assert!(self.setup.is_none() || self.lead_branch.is_some());
        if let (Some(_), Some(branch)) = (&self.setup, &self.lead_branch) {
            text.push_str(&format!(
                "(allow file-write*\n  (regex #\"^{}/\\.git/(logs/)?refs/heads/({}|{}--[a-z0-9-]+(\\.lock)?)$\"))\n",
                seatbelt_pattern(&self.project_root),
                regex_escape(self.hooks.linked_prefix.trim_end_matches('/')),
                regex_escape(branch)
            ));
        }
        assert!(self.links.len() <= LINKS_MAX);
        for link in self.links.iter().filter(|link| clean_absolute(link)) {
            text.push_str(&format!(
                "(allow file-read* (literal \"{}\"))\n",
                link.display()
            ));
        }
        text.push_str(&self.project_secret_rules());
        text.push_str("(deny file-read* file-write*");
        for secret in self.secret_paths() {
            if clean_absolute(&secret) {
                text.push_str(&format!("\n  (subpath \"{}\")", secret.display()));
            }
        }
        let credentials = CREDENTIAL_NAMES
            .iter()
            .map(|name| regex_escape(name))
            .collect::<Vec<_>>()
            .join("|");
        for root in &self.tool_roots {
            text.push_str(&format!(
                "\n  {}",
                at_or_below(&format!("{}/(.*/)?({credentials})", seatbelt_pattern(root)))
            ));
        }
        text.push_str(")\n(deny mach-lookup");
        for service in KEYCHAIN_SERVICES {
            text.push_str(&format!("\n  (global-name \"{service}\")"));
        }
        text.push_str(")\n");
        if let Some((home, folder)) = self
            .carved_home
            .as_ref()
            .filter(|(home, folder)| clean_absolute(home) && home.starts_with(folder))
        {
            assert!(self.engine_secrets.iter().any(|secret| secret == folder));
            text.push_str(&format!(
                "(allow file-read* file-write*\n  (subpath \"{}\"))\n(allow file-read-metadata",
                home.display()
            ));
            for above in home
                .ancestors()
                .skip(1)
                .take_while(|above| above.starts_with(folder))
            {
                text.push_str(&format!("\n  (literal \"{}\")", above.display()));
            }
            text.push_str(")\n");
        }
        if !self.hidden.is_empty() {
            text.push_str("(deny file-read* file-write* process-exec file-link file-clone");
            for folder in self.hidden.iter().filter(|folder| clean_absolute(folder)) {
                text.push_str(&format!("\n  (subpath \"{}\")", folder.display()));
            }
            text.push_str(")\n");
        }
        if !self.kept_out.is_empty() {
            text.push_str(KEPT_OUT_DENY);
            for path in self.kept_out.iter().filter(|path| clean_absolute(path)) {
                text.push_str(&format!("\n  (subpath \"{}\")", path.display()));
            }
            text.push_str(")\n");
        }
        text.push_str(&self.closed_tops_rule());
        for file in self.revealed.iter().filter(|file| clean_absolute(file)) {
            text.push_str(&format!(
                "(allow file-read* (literal \"{}\"))\n",
                file.display()
            ));
        }
        text.push_str(&self.project_denies());
        if let Some(scratch) = &self.scratch {
            text.push_str(&format!(
                "(deny {FIXED_WRITES}\n  (literal \"{}\"))\n",
                scratch.display()
            ));
        }
        if let Some(tmp) = &self.owner_tmp {
            let tmp = tmp.display();
            text.push_str(&format!(
                "(deny {FIXED_WRITES}\n  (literal \"{tmp}\"))\n(deny file-write*\n  (require-all\n    (subpath \"{tmp}\")\n    (vnode-type SOCKET)))\n"
            ));
        }
        text.push_str(&self.git_link_denies());
        text.push_str(&self.submodule_denies());
        text.push_str(&self.nested_git_denies());
        text.push_str(&self.ancestry_denies());
        if let Some(checkout) = &self.checkout {
            text.push_str(&checkout.object_denies());
            let [head, _] = ADMIN_WRITES.map(any_case);
            text.push_str(&format!(
                "(deny file-write*\n  (regex #\"^{}/{head}$\"))\n",
                seatbelt_pattern(&checkout.admin)
            ));
        }
        if let Some(clone) = &self.clone {
            text.push_str(&clone_store_denies(clone));
        }
        text.push_str(
            "(allow file-read*\n  (literal \"/dev\")\n  (literal \"/dev/random\")\n  (literal \"/dev/urandom\")\n  (literal \"/dev/stdin\")\n  (literal \"/dev/stdout\")\n  (literal \"/dev/stderr\"))\n(allow file-read* file-write-data file-ioctl\n  (literal \"/dev/null\")\n  (literal \"/dev/zero\")\n  (literal \"/dev/tty\")\n  (literal \"/dev/ptmx\")\n  (literal \"/dev/dtracehelper\")\n  (subpath \"/dev/fd\"))\n(allow file-read-metadata file-ioctl\n  (regex #\"^/dev/ttys[0-9]+$\"))\n(allow file-read* file-write-data\n  (require-all\n    (regex #\"^/dev/ttys[0-9]+$\")\n    (extension \"com.apple.sandbox.pty\")))\n",
        );
        for socket in &self.sockets {
            let path = socket.display();
            let folder = socket.parent().unwrap_or(socket).display();
            text.push_str(&format!(
                "(allow file-read* file-write*\n  (literal \"{path}\")\n  (literal \"{path}.lock\"))\n(allow file-read-metadata (literal \"{folder}\"))\n(allow network-bind (local unix-socket (path-literal \"{path}\")))\n(allow network-outbound (remote unix-socket (path-literal \"{path}\")))\n"
            ));
        }
        for socket in &self.served {
            let path = socket.display();
            let folder = socket.parent().unwrap_or(socket).display();
            text.push_str(&format!(
                "(allow file-read* file-write*\n  (literal \"{path}\")\n  (literal \"{path}.lock\"))\n(allow file-read-metadata (literal \"{folder}\"))\n(allow network-bind (local unix-socket (path-literal \"{path}\")))\n(deny file-write-unlink (literal \"{path}\"))\n"
            ));
        }
        for socket in self.reached.iter().chain(&self.granted) {
            let path = socket.display();
            let folder = socket.parent().unwrap_or(socket).display();
            text.push_str(&format!(
                "(allow file-read* (literal \"{path}\"))\n(allow file-read-metadata (literal \"{folder}\"))\n(allow network-outbound (remote unix-socket (path-literal \"{path}\")))\n(deny file-write* (literal \"{path}\"))\n"
            ));
        }
        for socket in &self.shut {
            let path = socket.display();
            text.push_str(&format!(
                "(deny file-write*\n  (literal \"{path}\")\n  (literal \"{path}.lock\"))\n"
            ));
        }
        for folder in &self.read_only {
            text.push_str(&format!(
                "(deny file-write* (subpath \"{}\"))\n",
                folder.display()
            ));
        }
        let launch = launch_denies(self.hooks, &self.agent_home);
        text.push_str(&launch);
        let config = if self.owner_config {
            String::new()
        } else {
            agent_config_denies(self.hooks, &self.agent_home)
        };
        text.push_str(&config);
        for root in self.writable_roots() {
            text.push_str(&root_pin(root));
        }
        for folder in &self.fixed {
            text.push_str(&format!(
                "(deny {FIXED_WRITES} (literal \"{}\"))\n",
                folder.display()
            ));
        }
        for folder in &self.shared {
            text.push_str(&shared_denies(folder));
        }
        let teammates = self.teammates_denies();
        text.push_str(&teammates);
        let answers = self.answers_denies();
        text.push_str(&answers);
        let agent_files = self
            .agent_files
            .map(|rule| rule.seatbelt(&self.project_root))
            .unwrap_or_default();
        text.push_str(&agent_files);
        let ignored = hfs_ignored_deny();
        text.push_str(&ignored);
        assert_eq!(
            text.matches("unix-socket").count(),
            2 * self.sockets.len() + self.served.len() + self.reached.len() + self.granted.len()
        );
        let mut last: Vec<String> = self
            .read_only
            .iter()
            .map(|folder| format!("(deny file-write* (subpath \"{}\"))", folder.display()))
            .collect();
        last.push(launch);
        if !config.is_empty() {
            last.push(config);
        }
        last.extend(self.writable_roots().map(|root| root_pin(root)));
        last.extend(
            self.fixed
                .iter()
                .map(|folder| format!("(deny {FIXED_WRITES} (literal \"{}\"))", folder.display())),
        );
        last.extend(self.shared.iter().map(|folder| shared_denies(folder)));
        if !teammates.is_empty() {
            last.push(teammates);
        }
        if !answers.is_empty() {
            last.push(answers);
        }
        if !agent_files.is_empty() {
            last.push(agent_files);
        }
        last.push(ignored);
        assert!(
            denies_follow_write_allows(&text, &last),
            "a read-only, root, fixed, shared or teammates' write deny must follow every write allow, since the last match wins"
        );
        let denied_last = |operations: &str, socket: &PathBuf| {
            text.rfind(&format!(
                "(deny {operations} (literal \"{}\"))",
                socket.display()
            )) > text.rfind("(allow file-write*")
        };
        assert!(
            self.served
                .iter()
                .all(|socket| denied_last("file-write-unlink", socket))
                && self
                    .reached
                    .iter()
                    .chain(&self.granted)
                    .all(|socket| denied_last("file-write*", socket)),
            "a served socket's unlink deny and a reached socket's write deny must follow every write allow, since the last match wins"
        );
        let network = text.len();
        match &self.network {
            SandboxNetwork::Open => {
                text.push_str(
                    "(allow network-outbound (remote ip \"*:*\"))\n(allow network-inbound (local ip \"*:*\"))\n(allow network-bind (local ip \"*:*\"))\n(allow network-outbound (literal \"/private/var/run/mDNSResponder\"))\n",
                );
                let owner = OWNER_PORTS
                    .iter()
                    .chain(&self.kept_ports)
                    .copied()
                    .chain(PROXY_PORTS)
                    .chain(RELAY_PORTS)
                    .map(|port| format!(" (local ip \"*:{port}\")"))
                    .collect::<String>();
                text.push_str(&format!(
                    "(deny network-bind{owner})\n(deny network-inbound{owner})\n"
                ));
                text.push_str(
                    "(allow system-socket (require-all (socket-domain AF_SYSTEM) (socket-protocol 2)))\n",
                );
            }
            SandboxNetwork::Only(remotes) => {
                for remote in remotes {
                    let target = match remote {
                        SandboxRemote::Loopback(port) => format!("localhost:{port}"),
                        SandboxRemote::AnyHost(port) => format!("*:{port}"),
                    };
                    text.push_str(&format!(
                        "(allow network-outbound (remote ip \"{target}\"))\n"
                    ));
                }
                if remotes
                    .iter()
                    .any(|remote| matches!(remote, SandboxRemote::AnyHost(_)))
                {
                    text.push_str(
                        "(allow network-outbound (literal \"/private/var/run/mDNSResponder\"))\n",
                    );
                }
            }
        }
        assert!(text.starts_with("(version 1)\n(deny default)\n"));
        assert!(
            text.find("(deny process-exec") > text.find("(allow process-exec)"),
            "the app bundle exec deny must follow the exec allow, since the last match wins"
        );
        assert!(!text.contains("(subpath \"/dev\")"));
        assert!(!text[network..].contains("network*") && !text[network..].contains("unix-socket"));
        assert!(
            MACH_SERVICES
                .iter()
                .all(|service| !KEYCHAIN_SERVICES.contains(service))
        );
        text
    }

    /// The Seatbelt rule that keeps the files the engine and the owner's git read in the
    /// project folder out of agent hands: both `context/` folders, the git hooks and the
    /// rebase, sequencer, remotes and branches state, each with everything below it, and the
    /// entries `.git`, `.crazyagent`, the project instructions file `.crazyagent/instructions.md`
    /// (SA vid 11), the git config and its lock, the index and its lock, `commondir`,
    /// `config.worktree`, `objects` and `objects/info`, none of which an agent may write or
    /// hard link under another name it could write through (SA vid 11 r2). Each name below the
    /// project folder matches in any letter case, so the rule does not depend on how Seatbelt
    /// folds case on the case-insensitive volume, where git finds a name however it is typed
    /// (RV-DS241-15). `.git/info`, `.git/shallow`, the alternates files and `.git/modules`
    /// are kept the same way by [`Self::ancestry_denies`] and [`Self::submodule_denies`].
    /// Input: the policy. Output: the rule text.
    fn project_denies(&self) -> String {
        let project = seatbelt_pattern(&self.project_root);
        let git = any_case(".git");
        let state = any_case(self.hooks.crazyagent_directory);
        let context = any_case("context");
        let objects = format!("{git}/{}", any_case("objects"));
        let mut below = vec![context.clone(), format!("{state}/{context}")];
        below.extend(GIT_STATE_TREES.map(|name| format!("{git}/{}", any_case(name))));
        let mut entries = vec![
            git.clone(),
            format!("{state}/{}", any_case(self.hooks.project_file)),
            state,
            format!("{objects}/{}", any_case("info")),
            objects,
        ];
        entries.extend(GIT_STATE_FILES.map(|name| format!("{git}/{}", any_case(name))));
        let mut text = String::from("(deny file-write* file-link");
        for path in &below {
            text.push_str(&format!(
                "\n  {}",
                at_or_below(&format!("{project}/{path}"))
            ));
        }
        for path in &entries {
            text.push_str(&format!("\n  (regex #\"^{project}/{path}$\")"));
        }
        text.push_str(")\n");
        assert_eq!(below.len() + entries.len(), 19);
        assert_eq!(text.matches("(regex #\"^/").count(), 19);
        text
    }

    /// The Seatbelt rules that keep the owner's git reading the metadata the engine made.
    /// No agent may move the project folder, the git worktrees folders or a worktree, which
    /// would carry protected files out of their denied paths and back. A worktree's `.git`
    /// file and its `commondir` and `gitdir` files may be created once, as `git worktree add`
    /// does, only by a profile that adds worktrees ([`Self::adding_worktrees`] or a lead's
    /// teammates' setup), since the owner's `git worktree repair` would point a real
    /// checkout at an admin folder planted with them; they are never rewritten, removed,
    /// re-permissioned, flagged, given an ACL or a time, or hard linked under a name an agent
    /// could change them through, so the owner's git keeps reading them (RV-DS241-185, rv-sa241
    /// NV-1, LINUX-TEAM-TIME-CALLS: a profile adding none denies every write, since Apple's
    /// `utimensat` sets times past `file-write-times`), and no `config.worktree` may be
    /// written. No link may be made anywhere in the git folder, in any letter case: git's
    /// `worktree prune`, which the owner's next commit runs as maintenance, empties whatever
    /// folder a link at `.git/worktrees` or `.git/worktrees/<name>` names
    /// (SECAUDIT-WORKTREE-ADMIN-PLANT). Each
    /// name below the project root, and each extra worktree's `.git` link, matches in any
    /// letter case, as in `clone_store_denies`, since the case-insensitive disk finds it
    /// however it is typed (DEC-123 rv-secreview L2, RV-DS241-15).
    /// Input: the policy. Output: the rule text.
    fn git_link_denies(&self) -> String {
        let project = self.project_root.display();
        let pattern = seatbelt_pattern(&self.project_root);
        let git = any_case(".git");
        let worktrees = any_case("worktrees");
        let state = any_case(self.hooks.crazyagent_directory);
        let admin = format!("{pattern}/{git}/{worktrees}/[^/]+");
        let checkouts = format!("{pattern}(/{state})?/{worktrees}/[^/]+");
        let mut moves = format!(
            "(deny file-write-unlink\n  (literal \"{project}\")\n  (regex #\"^{pattern}/{git}/{worktrees}$\")\n  (regex #\"^{pattern}(/{state})?/{worktrees}$\")\n  (regex #\"^{admin}$\")\n  (regex #\"^{checkouts}$\")"
        );
        let denied = if self.adds_worktrees || self.setup.is_some() {
            "file-write-unlink file-write-data file-write-mode file-write-flags file-write-acl file-write-times file-link"
        } else {
            "file-write* file-link"
        };
        let [commondir, gitdir] = ADMIN_LINKS.map(any_case);
        let mut links = format!(
            "(deny {denied}\n  (regex #\"^{admin}/({commondir}|{gitdir})$\")\n  (regex #\"^{checkouts}/{git}$\")"
        );
        for worktree in &self.worktrees {
            moves.push_str(&format!("\n  (literal \"{}\")", worktree.display()));
            links.push_str(&format!(
                "\n  (regex #\"^{}/{git}$\")",
                seatbelt_pattern(worktree)
            ));
        }
        let [_, worktree_config] = ADMIN_WRITES.map(any_case);
        let text = format!(
            "{moves})\n{links})\n(deny file-write*\n  (regex #\"^{admin}/{worktree_config}$\"))\n(deny file-write-create\n  (require-all (regex #\"^{pattern}/{git}/\") (vnode-type SYMLINK)))\n"
        );
        assert_eq!(
            text.matches("(regex #\"^/").count(),
            8 + self.worktrees.len()
        );
        assert!(text.contains("(vnode-type SYMLINK)"));
        assert!(!text.contains("/.crazyagent/worktrees\""));
        text
    }

    /// The Seatbelt rule that keeps the submodule store out of agent hands. Git keeps a
    /// submodule's own repository, with the config and hooks a later git would run, under
    /// `.git/modules`; no agent may write there, in the common git folder or a worktree
    /// admin folder, so none can plant a submodule whose metadata the engine or the owner
    /// then enters. The `.gitmodules` file and gitlinks stay writable tracked content: the
    /// engine's own git ignores them with `--ignore-submodules` and never recurses. The
    /// name matches in any letter case ([`any_case`], the long s included), since the
    /// project disk may be case insensitive. Input: the policy. Output: the rule text.
    fn submodule_denies(&self) -> String {
        let pattern = seatbelt_pattern(&self.project_root);
        let dot_git = any_case(".git");
        let worktrees = any_case("worktrees");
        let modules = any_case("modules");
        let text = format!(
            "(deny file-write*\n  {})\n",
            at_or_below(&format!(
                "{pattern}/{dot_git}(/{worktrees}/[^/]+)?/{modules}"
            ))
        );
        assert_eq!(text.matches("(regex #\"^/").count(), 1);
        text
    }

    /// The Seatbelt rule that keeps an agent from planting a nested git repository inside a
    /// writable working tree, which the owner's own unsandboxed git (its terminal or the
    /// V2 IDE) would later enter as a submodule and run under the agent's own config (SM1).
    /// No entry named `.git`, in every spelling a case-insensitive volume finds ([`any_case`]),
    /// as a folder, file or link, may be created below a writable working tree, except that
    /// tree's own top-level `.git` and, only in a profile built by [`Self::adding_worktrees`],
    /// the engine's worktree links, given back by their exact name in either case, which any
    /// other process could point at git data of its own (SECAUDIT-WORKTREE-ADMIN-PLANT).
    /// The project's own `.git` and each worktree's own `.git` sit
    /// at a tree's root, not below it, so the `.+/` guard never matches them. A whole folder
    /// already holding a `.git` could once be renamed in as a path-blind move Seatbelt
    /// cannot inspect; that source is removed by also denying `.git` at any depth, top level
    /// included, in every writable root that is not a working tree ([`Self::git_deny_roots`],
    /// the owner's temp folder and shared writable folders included), so no agent can create
    /// a repository anywhere to rename from (SM1-R, RV-DS241-16). The only `.git` entries in agent roots are engine-made links to exactly
    /// `/dev/null`: the uv markers in a tool cache's buckets (T8 C1), which an agent cannot
    /// make, remove or point elsewhere, and which, moved with their bucket into a tree, git
    /// never enters or stages. Input: the policy. Output: the rule text.
    fn nested_git_denies(&self) -> String {
        let dot_git = any_case(".git");
        let own_git = "\\.[gG][iI][tT]";
        let (working, other) = self.git_deny_roots();
        let mut nested = String::new();
        for root in &working {
            nested.push_str(&format!(
                "\n      {}",
                at_or_below(&format!("{}/.+/{dot_git}", seatbelt_pattern(root)))
            ));
        }
        for root in &other {
            nested.push_str(&format!("\n      {}", any_depth_git(root)));
        }
        let pattern = seatbelt_pattern(&self.project_root);
        let checkouts = format!("{pattern}(/\\.crazyagent)?/worktrees/[^/]+");
        let mut exempt = String::new();
        if self.adds_worktrees {
            exempt.push_str(&format!(
                "\n    (require-not (regex #\"^{checkouts}/{own_git}$\"))"
            ));
        }
        if let Some(clone) = &self.clone {
            exempt.push_str(&format!(
                "\n    (require-not (regex #\"^{}/{}/[^/]+/{own_git}$\"))",
                seatbelt_pattern(clone),
                regex_escape(self.hooks.teammates_folder)
            ));
        }
        if let Some(setup) = &self.setup {
            exempt.push_str(&format!(
                "\n    (require-not (regex #\"^{}/[^/]+/{own_git}$\"))",
                seatbelt_pattern(&setup.folder)
            ));
        }
        for worktree in &self.worktrees {
            exempt.push_str(&format!(
                "\n    (require-not (literal \"{}\"))",
                worktree.join(".git").display()
            ));
        }
        let text =
            format!("(deny file-write*\n  (require-all\n    (require-any{nested}){exempt}))\n");
        assert!(
            text.matches("(regex #\"^/").count()
                == working.len()
                    + other.len()
                    + usize::from(self.adds_worktrees)
                    + usize::from(self.clone.is_some())
                    + usize::from(self.setup.is_some())
        );
        assert!(text.ends_with(")\n"));
        text
    }

    /// The Seatbelt rule that keeps an agent from writing what git reads ancestry or objects
    /// from beside the commits, in every letter case the volume folds together (`any_case`,
    /// the Kelvin sign and long s included): a commit-graph file, single or split, and an
    /// `alternates` or `http-alternates` file in the project's object store or the one its
    /// checkout shares (land-19 r1 M1, r2 L1), and the project's `.git/info` (which holds
    /// `grafts`) and `.git/shallow` (land-19 r2 M1). A forged graph or graft could claim an
    /// agent commit is on the landing tip. This is the only rule on those names, and it holds
    /// a case variant made where the name does not exist yet, as after a clone or
    /// `init --template=`, which git then reads through the case-insensitive volume. Engine
    /// git reads no graph or graft; this also keeps the owner's own git from being handed one.
    /// Input: the policy. Output: the rule text.
    fn ancestry_denies(&self) -> String {
        let git = format!(
            "{}/{}",
            seatbelt_pattern(&self.project_root),
            any_case(".git")
        );
        let mut stores = vec![format!("{git}/{}", any_case("objects"))];
        if let Some(checkout) = &self.checkout {
            stores.push(seatbelt_pattern(&checkout.objects));
        }
        let info = any_case("info");
        let mut text = format!(
            "(deny file-write*\n  (regex #\"^{git}/({info}|{})$\")\n  (regex #\"^{git}/{info}/\")",
            any_case("shallow")
        );
        let [alternates, http_alternates] = STORE_ALTERNATES.map(any_case);
        let graph = format!("{}{}?", any_case("commit-graph"), any_case("s"));
        for store in &stores {
            text.push_str(&format!(
                "\n  (regex #\"^{store}/{info}/({graph}|{alternates}|{http_alternates})$\")\n  (regex #\"^{store}/{info}/{graph}/\")"
            ));
        }
        text.push_str(")\n");
        assert_eq!(text.matches("(regex #\"^/").count(), 2 * stores.len() + 2);
        text
    }

    /// Splits the agent's writable roots into the working-tree roots, where the root's own
    /// top-level `.git` is a legitimate repository link the engine adds, and every other
    /// writable root, where no `.git` may ever appear. The working trees are the project
    /// folder, worktree, clone or setup folder the agent works in and its extra worktrees;
    /// every other root comes from [`Self::writable_roots`] itself, so a root granted there
    /// is under the rule at once: an admin folder, object store or setup records, home, temp
    /// or scratch, a team folder a native lead shares with its teammates, the tool cache (T8,
    /// whose only `.git` entries are the engine's `/dev/null` links), the owner's temp folder
    /// and every shared writable folder (RV-DS241-16). A `.git` anywhere below a working
    /// tree, and a `.git` at any depth (top level included) in every other root, is denied so
    /// no agent can create a repository the owner's git enters there or rename one whole
    /// into a worktree (OP-9 r2, SM1-R). Output: the working roots and the other roots.
    pub fn git_deny_roots(&self) -> (Vec<&Path>, Vec<&Path>) {
        assert!(self.confinements() <= 1);
        let mut working: Vec<&Path> = vec![match (&self.checkout, &self.clone, &self.setup) {
            (Some(checkout), _, _) => checkout.worktree.as_path(),
            (None, Some(clone), _) => clone.as_path(),
            (None, None, Some(setup)) => setup.folder.as_path(),
            (None, None, None) => self.project_root.as_path(),
        }];
        working.extend(self.worktrees.iter().map(PathBuf::as_path));
        let other: Vec<&Path> = self
            .writable_roots()
            .map(PathBuf::as_path)
            .filter(|root| !working.contains(root))
            .collect();
        assert!(
            working
                .iter()
                .all(|root| self.writable_roots().any(|writable| writable == root))
        );
        assert!(other.len() >= 2);
        (working, other)
    }

    /// Writes the profile with mode 0600 at `path`, replacing any earlier one
    /// atomically. The path must lie outside every writable root, so no agent
    /// can rewrite the profile its next launch uses. A profile holding a string Seatbelt
    /// cannot read, or longer than [`PROFILE_BYTES_MAX`], is refused as `Invalid`, never written.
    /// An agent home first gets the folders its profile keeps it from making
    /// ([`make_config_parents`]), unless it lies outside the engine's data root
    /// ([`Self::anchor_home`]).
    #[cfg(unix)]
    pub fn install(&self, path: &Path) -> Result<(), SandboxError> {
        let parent = path
            .parent()
            .filter(|parent| clean_absolute(parent) && parent.is_dir())
            .ok_or_else(|| invalid("profile directory must exist"))?;
        let parent = parent.canonicalize()?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| name.ends_with(".sb") && name.len() <= 128)
            .ok_or_else(|| invalid("profile name must end in .sb"))?;
        let target = parent.join(name);
        if self.writable_roots().any(|root| target.starts_with(root)) {
            return Err(invalid(
                "profile must live outside the sandbox's writable roots",
            ));
        }
        if !self.owner_config && self.engine_home != Some(false) {
            make_config_parents(self.hooks, &self.agent_home, self.engine_home == Some(true))?;
        }
        let profile = self.profile();
        if !sbpl_overlong(&profile).is_empty() {
            return Err(invalid(
                "a path or name in the sandbox profile is longer than Seatbelt reads",
            ));
        }
        if profile.len() > PROFILE_BYTES_MAX {
            return Err(invalid("the sandbox profile is larger than its 64 KiB cap"));
        }
        let staging = parent.join(format!(".{name}.{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&staging)?;
        let written = file
            .write_all(profile.as_bytes())
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&staging, &target));
        if let Err(error) = written {
            let _ = fs::remove_file(&staging);
            return Err(error.into());
        }
        Ok(())
    }

    /// Refuses to write a Seatbelt profile, which Windows cannot load; Windows agents start
    /// through [`InstalledSandbox`] and the AppContainer boundary. Takes the path, unused.
    /// Returns `Unavailable` with [`crate::windows::SEATBELT_MISSING`].
    #[cfg(target_os = "windows")]
    pub fn install(&self, _path: &Path) -> Result<(), SandboxError> {
        Err(SandboxError::Unavailable(
            crate::windows::SEATBELT_MISSING.into(),
        ))
    }

    /// A command that runs `program` under the installed profile with a cleared
    /// environment: HOME and TMPDIR point at the agent's own dirs, and the
    /// process leads its own group so the whole tree can be stopped.
    #[cfg(unix)]
    pub fn command(&self, profile: &Path, program: &Path) -> Command {
        let base = self.base_env(false);
        let mut command = agent_command(
            profile,
            program,
            &borrowed(&base),
            std::iter::empty::<(&OsStr, Option<&OsStr>)>(),
        )
        .expect("the base agent environment holds no owner secret");
        command.current_dir(&self.project_root);
        command
    }

    /// The variables every command this sandbox builds starts with: HOME, TMPDIR (the short
    /// temp folder when granted, else the temp dir), PATH with the owner's rust proxies after
    /// any [`Self::allow_node_for`] folder, the C locale and [`crate::toolchains::agent_env`]. Only a
    /// harness's PATH starts with the agent's own `getconf` and `git`
    /// ([`crate::toolchains::with_agent_git`]). Takes whether it is a harness. Returns the variables.
    pub fn base_env(&self, harness: bool) -> Vec<(&'static str, OsString)> {
        let homes = crate::toolchains::ToolHomes::at(&self.owner_home);
        let tmp = self
            .owner_tmp
            .as_ref()
            .or(self.scratch.as_ref())
            .unwrap_or(&self.agent_tmp);
        let path = crate::toolchains::agent_path(&homes, AGENT_PATH);
        let path = match self.node_bin.as_deref().and_then(Path::to_str) {
            Some(bin) => format!("{bin}:{path}"),
            None => path,
        };
        let path = if harness {
            crate::toolchains::with_agent_git(self.hooks.toolchain, &self.agent_home, path)
        } else {
            path
        };
        let mut env = vec![
            ("HOME", self.agent_home.clone().into_os_string()),
            ("TMPDIR", tmp.clone().into_os_string()),
            ("PATH", OsString::from(path)),
            ("LANG", OsString::from("C")),
            ("LC_ALL", OsString::from("C")),
        ];
        env.extend(crate::toolchains::agent_env(&self.agent_home, &homes));
        if let Some((cache, uv)) = &self.tool_cache {
            if *uv {
                env.push((TOOL_CACHE_ENV[0], cache.join("uv").into_os_string()));
            }
            env.push((TOOL_CACHE_ENV[1], cache.join("pnpm-store").into_os_string()));
        }
        env
    }

    /// Rebuilds a harness launch command (absolute program, arguments,
    /// explicitly set environment, working directory inside the project) so it
    /// runs under the installed profile. HOME and TMPDIR always point at the
    /// agent's own dirs, whatever the harness set.
    pub fn wrap(&self, profile: &Path, harness: &Command) -> Result<Command, SandboxError> {
        let (program, cwd) = self.harness_start(harness)?;
        let base = self.base_env(true);
        #[cfg(unix)]
        let mut command = agent_command(profile, program, &borrowed(&base), harness.get_envs())?;
        #[cfg(target_os = "windows")]
        let mut command = (self.hooks.windows_agent_command)(
            profile,
            program,
            &borrowed(&base),
            harness.get_envs().collect(),
        )?;
        command.args(harness.get_args()).current_dir(cwd);
        assert!(starts_at_boundary(command.get_program()));
        Ok(command)
    }

    /// Rebuilds a harness launch command for a Full access agent (DEC-102) as [`Self::wrap`]
    /// does, with the same checks and environment, but started by [`unconfined_command`]
    /// with no Seatbelt profile. Input: the harness command. Output: the command, or
    /// `Invalid` for an unsafe command.
    #[cfg(unix)]
    pub fn wrap_unconfined(&self, harness: &Command) -> Result<Command, SandboxError> {
        let (program, cwd) = self.harness_start(harness)?;
        let base = self.base_env(false);
        let mut command = unconfined_command(program, &borrowed(&base), harness.get_envs())?;
        command.args(harness.get_args()).current_dir(cwd);
        assert_ne!(command.get_program(), SANDBOX_EXEC);
        Ok(command)
    }

    /// Refuses a Full access harness command on Windows, where the level is unavailable.
    /// Input: the harness command. Output: always `Unavailable`.
    #[cfg(not(unix))]
    pub fn wrap_unconfined(&self, harness: &Command) -> Result<Command, SandboxError> {
        let _ = harness;
        Err(SandboxError::Unavailable(FULL_ACCESS_NEEDS_MACOS.into()))
    }

    fn harness_start<'a>(
        &'a self,
        harness: &'a Command,
    ) -> Result<(&'a Path, &'a Path), SandboxError> {
        let program = Path::new(harness.get_program());
        if !clean_absolute(program) {
            return Err(invalid("harness program must be an absolute plain path"));
        }
        let cwd = harness.get_current_dir().unwrap_or(&self.project_root);
        if !clean_absolute(cwd)
            || !std::iter::once(&self.project_root)
                .chain(self.worktrees.iter())
                .any(|root| cwd.starts_with(root))
        {
            return Err(invalid(
                "harness must start inside the project folder or its worktree",
            ));
        }
        Ok((program, cwd))
    }
}
