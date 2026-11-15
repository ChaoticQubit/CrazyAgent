use super::*;
use std::process::Command;
use std::time::{Duration, Instant};

const GIT: &str = "/usr/bin/git";
const HOST: &str = "github.com";
const OWNER: &str = "owner@example.invalid";
const TOKEN: &str = "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";
const MESSAGE: &str = "Publish with CrazyAgent";
const LIMIT: Duration = Duration::from_secs(2);

fn git_by(email: &str, args: &[&str], folder: &Path) -> String {
    let output = crate::spawn_gate::output(
        Command::new(GIT)
            .args(args)
            .current_dir(folder)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", folder)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Someone")
            .env("GIT_AUTHOR_EMAIL", email)
            .env("GIT_COMMITTER_NAME", "Someone")
            .env("GIT_COMMITTER_EMAIL", email),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn git(args: &[&str], folder: &Path) -> String {
    git_by(OWNER, args, folder)
}

struct Fixture {
    project: tempfile::TempDir,
    home: tempfile::TempDir,
}

fn fixture() -> Fixture {
    Fixture {
        project: tempfile::tempdir().unwrap(),
        home: tempfile::tempdir().unwrap(),
    }
}

fn ada() -> Author {
    Author {
        name: "Ada Lovelace".into(),
        email: "ada@example.invalid".into(),
    }
}

fn finding(path: &str, line: u32, kind: GitHubSecretKind) -> GitHubSecretFinding {
    GitHubSecretFinding {
        path: path.into(),
        line,
        kind,
    }
}

impl Fixture {
    fn path(&self) -> PathBuf {
        self.project.path().canonicalize().unwrap()
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.path().join(relative)).unwrap()
    }

    fn inspect(&self) -> Result<GitHubLocal, EngineError> {
        inspect(Path::new(GIT), self.home.path(), &self.path(), HOST)
    }

    fn prepare(
        &self,
        local: &GitHubLocal,
        private: bool,
        docs: bool,
        approved: &[&str],
    ) -> Result<Vec<String>, EngineError> {
        let approved: Vec<String> = approved.iter().map(|line| (*line).to_owned()).collect();
        prepare(
            Path::new(GIT),
            self.home.path(),
            &self.path(),
            local,
            &GitHubPrepare {
                private,
                docs,
                approved,
            },
        )
    }

    fn findings(&self, history: GitHubHistory) -> Result<Vec<GitHubSecretFinding>, EngineError> {
        self.scan(history).map(|scan| scan.findings)
    }

    fn scan(&self, history: GitHubHistory) -> Result<GitHubSecretScan, EngineError> {
        secret_findings(Path::new(GIT), self.home.path(), &self.path(), history)
    }

    fn commit(&self, written: &[&str]) -> Result<GitHubCommitted, EngineError> {
        let written: Vec<String> = written.iter().map(|path| (*path).to_owned()).collect();
        commit(
            Path::new(GIT),
            self.home.path(),
            &self.path(),
            &written,
            &ada(),
        )
    }

    fn lines(&self, args: &[&str]) -> Vec<String> {
        git(args, &self.path()).lines().map(str::to_owned).collect()
    }

    fn published(&self, private: bool, approved: &[&str]) -> GitHubCommitted {
        let local = self.inspect().unwrap();
        let written = self.prepare(&local, private, false, approved).unwrap();
        let written: Vec<&str> = written.iter().map(String::as_str).collect();
        self.commit(&written).unwrap()
    }
}

#[test]
fn inspect_reads_the_repository_state_and_refuses_a_remote_or_a_detached_head() {
    let fresh = fixture();
    assert_eq!(
        fresh.inspect().unwrap(),
        GitHubLocal {
            repository: false,
            branch: None,
            history: GitHubHistory::Empty,
        }
    );
    let unborn = fixture();
    git(&["init", "-q", "-b", "main"], &unborn.path());
    assert_eq!(
        unborn.inspect().unwrap(),
        GitHubLocal {
            repository: true,
            branch: Some("main".into()),
            history: GitHubHistory::Empty,
        }
    );
    let engine = fixture();
    git(&["init", "-q"], &engine.path());
    git_by(
        "engine@crazyagent.invalid",
        &["commit", "-q", "--allow-empty", "-m", "Start project"],
        &engine.path(),
    );
    assert_eq!(
        engine.inspect().unwrap(),
        GitHubLocal {
            repository: true,
            branch: Some("master".into()),
            history: GitHubHistory::EngineStart,
        }
    );
    git(
        &["commit", "-q", "--allow-empty", "-m", "Owner work"],
        &engine.path(),
    );
    assert_eq!(engine.inspect().unwrap().history, GitHubHistory::Owner);
    let posed = fixture();
    git(&["init", "-q"], &posed.path());
    git(
        &["commit", "-q", "--allow-empty", "-m", "Start project"],
        &posed.path(),
    );
    assert_eq!(posed.inspect().unwrap().history, GitHubHistory::Owner);
    let forged = fixture();
    git(&["init", "-q"], &forged.path());
    forged.write("keys.txt", &format!("{TOKEN}\n"));
    git(&["add", "-A"], &forged.path());
    git_by(
        "engine@crazyagent.invalid",
        &["commit", "-q", "-m", "Start project"],
        &forged.path(),
    );
    assert_eq!(forged.inspect().unwrap().history, GitHubHistory::Owner);
    let owned = fixture();
    git(&["init", "-q", "-b", "trunk"], &owned.path());
    owned.write("a.txt", "a\n");
    git(&["add", "-A"], &owned.path());
    git(&["commit", "-q", "-m", "first"], &owned.path());
    assert_eq!(
        owned.inspect().unwrap(),
        GitHubLocal {
            repository: true,
            branch: Some("trunk".into()),
            history: GitHubHistory::Owner,
        }
    );
    git(&["checkout", "-q", "--detach"], &owned.path());
    assert_eq!(owned.inspect().unwrap_err().0, "github_publish_detached");
    for (name, url, code) in [
        (
            "origin",
            "https://gitlab.com/o/r.git",
            "github_publish_other_host",
        ),
        (
            "origin",
            "https://notgithub.com/o/r.git",
            "github_publish_other_host",
        ),
        (
            "origin",
            "git@github.com:o/r.git",
            "github_publish_on_github",
        ),
        (
            "upstream",
            "https://github.com/o/r",
            "github_publish_on_github",
        ),
        (
            "origin",
            "ssh://git@ssh.github.com:443/o/r.git",
            "github_publish_on_github",
        ),
        ("backup", "/srv/git/r.git", "github_publish_other_host"),
    ] {
        let remote = fixture();
        git(&["init", "-q", "-b", "main"], &remote.path());
        git(&["remote", "add", name, url], &remote.path());
        assert_eq!(remote.inspect().unwrap_err().0, code, "{url}");
    }
    let file = fixture();
    file.write(".git", "gitdir: /elsewhere\n");
    assert_eq!(
        file.inspect().unwrap_err().0,
        "github_publish_not_plain_repo"
    );
    let linked = fixture();
    let real = fixture();
    git(&["init", "-q", "-b", "main"], &real.path());
    std::os::unix::fs::symlink(real.path().join(".git"), linked.path().join(".git")).unwrap();
    assert_eq!(
        linked.inspect().unwrap_err().0,
        "github_publish_not_plain_repo"
    );
    let not_folder = fixture();
    not_folder.write("plain.txt", "x\n");
    assert_eq!(
        inspect(
            Path::new(GIT),
            not_folder.home.path(),
            &not_folder.path().join("plain.txt"),
            HOST
        )
        .unwrap_err()
        .0,
        "project_folder_missing"
    );
}

#[test]
fn prepare_initializes_main_hides_crazyagent_and_appends_the_ignore_lines_once() {
    let fixture = fixture();
    let project = fixture.path();
    fixture.write(".crazyagent/context/roadmap.md", "plan\n");
    fixture.write(".crazyagent/worktrees/agent/x.rs", "fn x() {}\n");
    fixture.write("src/main.rs", "fn main() {}\n");
    fixture.write(".gitignore", "node_modules");
    let local = fixture.inspect().unwrap();
    let written = fixture
        .prepare(&local, true, false, &["target/", ".env*", "node_modules"])
        .unwrap();
    assert_eq!(written, [".gitignore"]);
    assert_eq!(
        git(&["symbolic-ref", "HEAD"], &project),
        "refs/heads/main\n"
    );
    let exclude = fixture.read(".git/info/exclude");
    assert!(
        exclude.lines().any(|line| line == ".crazyagent/"),
        "{exclude}"
    );
    assert!(
        exclude.lines().any(|line| line == "graphify-out/"),
        "{exclude}"
    );
    let ignore = "node_modules\n/.crazyagent/\n/graphify-out/\ntarget/\n.env*\n";
    assert_eq!(fixture.read(".gitignore"), ignore);
    let again = fixture.inspect().unwrap();
    assert_eq!(
        again,
        GitHubLocal {
            repository: true,
            branch: Some("main".into()),
            history: GitHubHistory::Empty,
        }
    );
    assert!(
        fixture
            .prepare(&again, true, false, &["target/"])
            .unwrap()
            .is_empty()
    );
    assert_eq!(fixture.read(".gitignore"), ignore);
    for bad in ["two\nlines", "", "tab\tline", "x".repeat(1025).as_str()] {
        assert_eq!(
            fixture.prepare(&again, true, false, &[bad]).unwrap_err().0,
            "github_publish_ignore_invalid",
            "{bad:?}"
        );
    }
    assert_eq!(fixture.read(".gitignore"), ignore);
    let linked = fixture();
    let outside = fixture();
    outside.write("kept", "keep\n");
    std::os::unix::fs::symlink(
        outside.path().join("kept"),
        linked.path().join(".gitignore"),
    )
    .unwrap();
    let local = linked.inspect().unwrap();
    assert_eq!(
        linked.prepare(&local, true, false, &[]).unwrap_err().0,
        "github_publish_gitignore_invalid"
    );
    assert_eq!(outside.read("kept"), "keep\n");
}

#[test]
fn the_docs_switch_decides_whether_the_context_folder_is_listed() {
    for (docs, expected) in [
        (false, vec![".gitignore", "src/a.rs"]),
        (
            true,
            vec![".crazyagent/context/roadmap.md", ".gitignore", "src/a.rs"],
        ),
    ] {
        let fixture = fixture();
        crate::projects_home::prepare_layout(&fixture.path()).unwrap();
        fixture.write(".crazyagent/context/roadmap.md", "plan\n");
        fixture.write(".crazyagent/research/r.md", "notes\n");
        fixture.write(".crazyagent/worktrees/agent/x.rs", "x\n");
        fixture.write(".crazyagent/instructions.md", "be careful\n");
        fixture.write("graphify-out/graph.json", "{}\n");
        fixture.write("src/a.rs", "a\n");
        let local = fixture.inspect().unwrap();
        fixture.prepare(&local, true, docs, &[]).unwrap();
        let listed = git(
            &["ls-files", "--others", "--exclude-standard"],
            &fixture.path(),
        );
        assert_eq!(listed.lines().collect::<Vec<_>>(), expected, "docs {docs}");
    }
    let older = fixture();
    older.write("context/plan.md", "mine\n");
    older.write("worktrees/agent/x.rs", "x\n");
    older.write("graphify-out/graph.json", "{}\n");
    older.write("src/a.rs", "a\n");
    let local = older.inspect().unwrap();
    older.prepare(&local, true, true, &[]).unwrap();
    let listed = git(
        &["ls-files", "--others", "--exclude-standard"],
        &older.path(),
    );
    assert_eq!(
        listed.lines().collect::<Vec<_>>(),
        [".gitignore", "context/plan.md", "src/a.rs"]
    );
    assert_eq!(older.read(".gitignore"), "/graphify-out/\n");
}

#[test]
fn generated_files_are_written_only_where_missing_and_never_through_a_link() {
    let public = fixture();
    for (path, text) in [
        ("Cargo.toml", "[package]\n"),
        ("package.json", "{}\n"),
        ("pyproject.toml", "[project]\n"),
        ("go.mod", "module x\n"),
        (".github/workflows/ci.yml", "on: push\n"),
    ] {
        public.write(path, text);
    }
    let local = public.inspect().unwrap();
    assert_eq!(
        public.prepare(&local, false, false, &[]).unwrap(),
        [".gitignore", "SECURITY.md", ".github/dependabot.yml"]
    );
    assert_eq!(
        public.read("SECURITY.md"),
        "# Security Policy\n\n## Reporting a Vulnerability\n\nPlease report a vulnerability privately through GitHub's private vulnerability reporting: open this repository's Security tab and choose \"Report a vulnerability\". Please do not open a public issue for it.\n"
    );
    let mut dependabot = String::from("version: 2\nupdates:\n");
    for ecosystem in ["cargo", "npm", "pip", "gomod", "github-actions"] {
        dependabot.push_str(&format!(
            "  - package-ecosystem: \"{ecosystem}\"\n    directory: \"/\"\n    schedule:\n      interval: \"weekly\"\n"
        ));
    }
    assert_eq!(public.read(".github/dependabot.yml"), dependabot);
    let private = fixture();
    private.write("requirements.txt", "requests\n");
    let local = private.inspect().unwrap();
    assert_eq!(
        private.prepare(&local, true, false, &[]).unwrap(),
        [".gitignore", ".github/dependabot.yml"]
    );
    assert!(!private.path().join("SECURITY.md").exists());
    assert!(private.read(".github/dependabot.yml").contains("\"pip\""));
    let kept = fixture();
    kept.write(".github/SECURITY.md", "ours\n");
    kept.write(".github/dependabot.yaml", "version: 2\n");
    kept.write("Cargo.toml", "[package]\n");
    let local = kept.inspect().unwrap();
    assert_eq!(
        kept.prepare(&local, false, false, &[]).unwrap(),
        [".gitignore"]
    );
    assert_eq!(kept.read(".github/SECURITY.md"), "ours\n");
    assert_eq!(kept.read(".github/dependabot.yaml"), "version: 2\n");
    assert!(!kept.path().join("SECURITY.md").exists());
    assert!(!kept.path().join(".github/dependabot.yml").exists());
    let plain = fixture();
    plain.write("README.md", "hello\n");
    let local = plain.inspect().unwrap();
    assert_eq!(
        plain.prepare(&local, true, false, &[]).unwrap(),
        [".gitignore"]
    );
    assert!(!plain.path().join(".github").exists());
    let outside = fixture();
    let linked = fixture();
    linked.write("Cargo.toml", "[package]\n");
    std::os::unix::fs::symlink(outside.path(), linked.path().join(".github")).unwrap();
    let local = linked.inspect().unwrap();
    assert_eq!(
        linked.prepare(&local, true, false, &[]).unwrap_err().0,
        "github_publish_file_invalid"
    );
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    assert!(!linked.path().join(".gitignore").exists());
    assert!(!linked.path().join(".git").exists());
    let dangling = fixture();
    std::os::unix::fs::symlink(
        outside.path().join("x"),
        dangling.path().join("SECURITY.md"),
    )
    .unwrap();
    let local = dangling.inspect().unwrap();
    assert_eq!(
        dangling.prepare(&local, false, false, &[]).unwrap(),
        [".gitignore"]
    );
    assert!(!outside.path().join("x").exists());
}

#[test]
fn secret_findings_name_file_and_line_and_pass_over_lookalikes() {
    let fixture = fixture();
    fixture.write(
        "src/app.rs",
        &format!("fn main() {{\n    let key = \"{TOKEN}\";\n}}\n"),
    );
    fixture.write(
        "keys/deploy",
        "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\n",
    );
    fixture.write(
        ".env",
        "PORT=8080\nDATABASE_PASSWORD=hunter22\nexport API_TOKEN='s3cr3tV4lue'\n",
    );
    fixture.write(
        "config/app.yaml",
        "server:\n  sort_key: created_at\n  auth: true\n  password: \"Tr0ub4dor&3\"\n",
    );
    fixture.write(
        ".npmrc",
        "//registry.npmjs.org/:_authToken=npm_A1b2C3d4E5f6G7h8\n",
    );
    fixture.write(
        "web/session.js",
        "const jwt = 'eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U';\n",
    );
    fixture.write(
        "src/lexer.rs",
        "let token = lexer.next();\nprimary_key=True\nlet model = \"sk-learn-something-long-name\";\nfn pk_test_function_name_2() {}\n//# sourceMappingURL=data:application/json;base64,eyJ2ZXJzaW9uIjozLCJzb3VyY2VzIjpbImEuanMiXSwibWFwcGluZ3MiOiJBQUFBIn0=\n",
    );
    fixture.write("main.py", "password = \"hunter22\"\n");
    fixture.write(
        ".env.example",
        "API_KEY=your-key-here\nSECRET=changeme\nTOKEN=${TOKEN}\nPASSWORD=\nDB_PASS=<password>\n",
    );
    fixture.write(
        "Cargo.lock",
        "checksum = \"a3f1c0e5b2d4a3f1c0e5b2d4a3f1c0e5b2d4a3f1c0e5b2d4a3f1c0e5b2d4a3f1\"\n",
    );
    fixture.write("ignored.txt", &format!("{TOKEN}\n"));
    fixture.write(".gitignore", "ignored.txt\n");
    std::fs::write(
        fixture.path().join("blob.bin"),
        [b"\0\0".as_slice(), TOKEN.as_bytes()].concat(),
    )
    .unwrap();
    git(&["init", "-q", "-b", "main"], &fixture.path());
    assert_eq!(
        fixture.findings(GitHubHistory::Empty).unwrap(),
        [
            finding(".env", 2, GitHubSecretKind::NamedValue),
            finding(".env", 3, GitHubSecretKind::NamedValue),
            finding(".npmrc", 1, GitHubSecretKind::NamedValue),
            finding("config/app.yaml", 4, GitHubSecretKind::NamedValue),
            finding("keys/deploy", 1, GitHubSecretKind::PrivateKey),
            finding("src/app.rs", 2, GitHubSecretKind::Token),
            finding("web/session.js", 1, GitHubSecretKind::Token),
        ]
    );
}

#[test]
fn owner_history_is_scanned_from_heads_tree_and_the_scan_is_bounded() {
    let fixture = fixture();
    let project = fixture.path();
    git(&["init", "-q", "-b", "main"], &project);
    fixture.write("notes.txt", &format!("first\n{TOKEN}\n"));
    fixture.write("clean.txt", "nothing\n");
    git(&["add", "-A"], &project);
    git(&["commit", "-q", "-m", "owner"], &project);
    fixture.write("notes.txt", "first\nremoved\n");
    fixture.write("new.txt", &format!("{TOKEN}\n"));
    assert_eq!(fixture.inspect().unwrap().history, GitHubHistory::Owner);
    assert_eq!(
        fixture.findings(GitHubHistory::Owner).unwrap(),
        [finding("notes.txt", 2, GitHubSecretKind::Token)]
    );
    assert_eq!(
        fixture.findings(GitHubHistory::Empty).unwrap(),
        [finding("new.txt", 1, GitHubSecretKind::Token)]
    );
    let home = fixture.home.path();
    let within = |history, files, bytes| {
        findings_within(Path::new(GIT), home, &project, history, files, bytes, LIMIT)
    };
    for (history, files, bytes) in [
        (GitHubHistory::Owner, 1, 1 << 20),
        (GitHubHistory::Owner, 10, 8),
        (GitHubHistory::Empty, 2, 1 << 20),
        (GitHubHistory::EngineStart, 10, 8),
    ] {
        assert_eq!(
            within(history, files, bytes).unwrap_err().0,
            "github_publish_too_large",
            "{history:?} {files} {bytes}"
        );
    }
    assert_eq!(
        within(GitHubHistory::Owner, 2, 1 << 20)
            .unwrap()
            .findings
            .len(),
        1
    );
}

#[test]
fn commit_takes_everything_kept_when_the_history_is_empty_or_only_the_engine_start() {
    let fresh = fixture();
    fresh.write("src/a.rs", "a\n");
    fresh.write(".env", "PORT=1\n");
    fresh.write(".crazyagent/context/roadmap.md", "plan\n");
    let committed = fresh.published(true, &[".env*"]);
    assert_eq!(committed.branch, "main");
    assert_eq!(committed.left_out, 0);
    assert_eq!(committed.commit, fresh.lines(&["rev-parse", "HEAD"])[0]);
    assert_eq!(
        fresh.lines(&["log", "--format=%an|%ae|%cn|%ce|%s"]),
        [format!(
            "Ada Lovelace|ada@example.invalid|Ada Lovelace|ada@example.invalid|{MESSAGE}"
        )]
    );
    assert_eq!(
        fresh.lines(&["ls-tree", "-r", "--name-only", "HEAD"]),
        [".gitignore", "src/a.rs"]
    );
    assert!(fresh.lines(&["status", "--porcelain"]).is_empty());
    let engine = fixture();
    git(&["init", "-q"], &engine.path());
    git_by(
        "engine@crazyagent.invalid",
        &["commit", "-q", "--allow-empty", "-m", "Start project"],
        &engine.path(),
    );
    engine.write("b.txt", "b\n");
    let committed = engine.published(true, &[]);
    assert_eq!(committed.branch, "master");
    assert_eq!(
        engine.lines(&["log", "--format=%s"]),
        [MESSAGE, "Start project"]
    );
    assert_eq!(
        engine.lines(&["ls-tree", "-r", "--name-only", "HEAD"]),
        [".gitignore", "b.txt"]
    );
    let unborn = fixture();
    git(&["init", "-q", "-b", "main"], &unborn.path());
    unborn.write("c.txt", "c\n");
    let committed = unborn.published(true, &[]);
    assert_eq!(committed.branch, "main");
    assert_eq!(unborn.lines(&["log", "--format=%s"]), [MESSAGE]);
    assert_eq!(
        unborn.lines(&["ls-tree", "-r", "--name-only", "HEAD"]),
        [".gitignore", "c.txt"]
    );
}

#[test]
fn commit_on_owner_history_takes_only_the_files_publish_wrote() {
    let owned = fixture();
    let project = owned.path();
    git(&["init", "-q", "-b", "trunk"], &project);
    owned.write("a.txt", "a\n");
    owned.write("Cargo.toml", "[package]\n");
    git(&["add", "-A"], &project);
    git(&["commit", "-q", "-m", "first"], &project);
    owned.write("a.txt", "changed\n");
    owned.write("staged.txt", "staged\n");
    git(&["add", "staged.txt"], &project);
    owned.write("u.txt", "untracked\n");
    let committed = owned.published(false, &[]);
    assert_eq!(committed.branch, "trunk");
    assert_eq!(committed.left_out, 3);
    assert_eq!(
        owned.lines(&["log", "--format=%an|%s"]),
        [format!("Ada Lovelace|{MESSAGE}"), "Someone|first".into()]
    );
    assert_eq!(
        owned.lines(&["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]),
        [".github/dependabot.yml", ".gitignore", "SECURITY.md"]
    );
    assert_eq!(
        owned.lines(&["diff", "--cached", "--name-only"]),
        ["staged.txt"]
    );
    assert_eq!(owned.lines(&["diff", "--name-only"]), ["a.txt"]);
    assert_eq!(owned.read("a.txt"), "changed\n");
    assert_eq!(owned.commit(&[]).unwrap(), committed);
    for bad in ["src/evil.rs", "../x", ".git/config", "u.txt"] {
        assert_eq!(
            owned.commit(&[bad]).unwrap_err().0,
            "github_publish_file_invalid",
            "{bad:?}"
        );
    }
    assert_eq!(
        owned.lines(&["rev-parse", "HEAD"]),
        [committed.commit.clone()]
    );
    git(&["checkout", "-q", "--detach"], &project);
    assert_eq!(owned.commit(&[]).unwrap_err().0, "github_publish_detached");
}

#[test]
fn the_author_is_the_owners_git_identity_else_the_github_noreply_address() {
    let fixture = fixture();
    let project = fixture.path();
    git(&["init", "-q", "-b", "main"], &project);
    let owner_home = tempfile::tempdir().unwrap();
    let author = || configured_author(Path::new(GIT), owner_home.path(), &project);
    assert_eq!(author(), None);
    std::fs::write(
        owner_home.path().join(".gitconfig"),
        "[user]\n\tname = Grace Hopper\n\temail = grace@example.invalid\n",
    )
    .unwrap();
    assert_eq!(
        author(),
        Some(Author {
            name: "Grace Hopper".into(),
            email: "grace@example.invalid".into(),
        })
    );
    git(&["config", "user.name", "Ada Lovelace"], &project);
    git(&["config", "user.email", "ada@example.invalid"], &project);
    assert_eq!(author(), Some(ada()));
    for bad in ["not an email", "<ada@example.invalid>"] {
        git(&["config", "user.email", bad], &project);
        assert_eq!(author(), None, "{bad}");
    }
    assert_eq!(
        noreply_author("octo-cat", Some(583_231)),
        Author {
            name: "octo-cat".into(),
            email: "583231+octo-cat@users.noreply.github.com".into(),
        }
    );
    assert_eq!(
        noreply_author("octo-cat", None).email,
        "octo-cat@users.noreply.github.com"
    );
}

fn mkfifo(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let status = crate::spawn_gate::status(Command::new("/usr/bin/mkfifo").arg(path)).unwrap();
    assert!(status.success());
}

#[test]
fn secret_findings_stop_at_the_bound_and_say_the_list_was_cut() {
    let one = fixture();
    git(&["init", "-q", "-b", "main"], &one.path());
    one.write("clean.txt", "nothing\n");
    assert_eq!(
        one.scan(GitHubHistory::Empty).unwrap(),
        GitHubSecretScan {
            findings: Vec::new(),
            truncated: false,
        }
    );
    one.write("many.rs", &format!("{TOKEN}\n").repeat(25));
    let scan = one.scan(GitHubHistory::Empty).unwrap();
    assert!(scan.truncated);
    assert_eq!(
        scan.findings,
        (1..=20)
            .map(|line| finding("many.rs", line, GitHubSecretKind::Token))
            .collect::<Vec<_>>()
    );
    let full = fixture();
    git(&["init", "-q", "-b", "main"], &full.path());
    for index in 0..60 {
        full.write(&format!("f{index:02}.rs"), &format!("{TOKEN}\n").repeat(25));
    }
    let scan = full.scan(GitHubHistory::Empty).unwrap();
    assert!(scan.truncated);
    assert_eq!(scan.findings.len(), 1000);
    assert!(scan.findings.iter().all(|found| found.line <= 20));
}

#[test]
fn a_fifo_in_the_project_or_its_config_never_holds_publish() {
    let ignore = fixture();
    git(&["init", "-q", "-b", "main"], &ignore.path());
    ignore.write("src/a.rs", "a\n");
    mkfifo(&ignore.path().join("sub/.gitignore"));
    let started = Instant::now();
    assert_eq!(
        findings_within(
            Path::new(GIT),
            ignore.home.path(),
            &ignore.path(),
            GitHubHistory::Empty,
            SCAN_FILES_MAX,
            SCAN_BYTES_MAX,
            LIMIT,
        )
        .unwrap_err()
        .0,
        "github_publish_failed"
    );
    assert!(started.elapsed() < Duration::from_secs(30));
    let included = fixture();
    let project = included.path();
    git(&["init", "-q", "-b", "main"], &project);
    let fifo = included.home.path().join("hang");
    mkfifo(&fifo);
    let config = project.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(&format!("[include]\n\tpath = {}\n", fifo.display()));
    std::fs::write(&config, text).unwrap();
    let started = Instant::now();
    assert_eq!(
        inspect_within(Path::new(GIT), included.home.path(), &project, HOST, LIMIT)
            .unwrap_err()
            .0,
        "github_publish_failed"
    );
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[test]
fn the_scan_needs_a_plain_git_folder_and_never_reads_through_a_linked_folder() {
    let linked = fixture();
    let project = linked.path();
    git(&["init", "-q", "-b", "main"], &project);
    linked.write("dir/secret.txt", "nothing\n");
    git(&["add", "dir/secret.txt"], &project);
    std::fs::remove_dir_all(project.join("dir")).unwrap();
    let outside = fixture();
    outside.write("secret.txt", &format!("{TOKEN}\n"));
    std::os::unix::fs::symlink(outside.path(), project.join("dir")).unwrap();
    linked.write("clean.txt", "nothing\n");
    assert_eq!(
        linked.findings(GitHubHistory::Empty).unwrap(),
        Vec::<GitHubSecretFinding>::new()
    );
    let file = fixture();
    file.write(".git", "gitdir: /elsewhere\n");
    file.write("a.txt", &format!("{TOKEN}\n"));
    assert_eq!(
        file.findings(GitHubHistory::Empty).unwrap_err().0,
        "github_publish_not_plain_repo"
    );
}

#[test]
fn docs_off_after_an_earlier_on_takes_the_context_folder_out_again() {
    let fixture = fixture();
    let project = fixture.path();
    crate::projects_home::prepare_layout(&project).unwrap();
    fixture.write(".crazyagent/context/roadmap.md", "plan\n");
    fixture.write("src/a.rs", "a\n");
    let listed = || {
        git(&["ls-files", "--others", "--exclude-standard"], &project)
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    for (docs, context) in [(false, false), (true, true), (false, false), (true, true)] {
        let local = fixture.inspect().unwrap();
        fixture.prepare(&local, true, docs, &[]).unwrap();
        assert_eq!(
            listed().contains(&".crazyagent/context/roadmap.md".to_owned()),
            context,
            "docs {docs}: {:?}",
            listed()
        );
    }
    let local = fixture.inspect().unwrap();
    fixture.prepare(&local, true, false, &[]).unwrap();
    assert_eq!(
        fixture.read(".gitignore"),
        "/.crazyagent/\n/graphify-out/\n"
    );
    assert_eq!(listed(), [".gitignore", "src/a.rs"]);
}

#[test]
fn a_gitignore_publish_cannot_read_is_refused_before_anything_is_written() {
    for bytes in [vec![0xff, b'\n'], vec![b'x'; (1 << 20) + 1]] {
        let fixture = fixture();
        std::fs::write(fixture.path().join(".gitignore"), &bytes).unwrap();
        fixture.write("a.txt", "a\n");
        let local = fixture.inspect().unwrap();
        assert_eq!(
            fixture.prepare(&local, false, false, &[]).unwrap_err().0,
            "github_publish_gitignore_invalid"
        );
        assert!(!fixture.path().join(".git").exists());
        assert!(!fixture.path().join("SECURITY.md").exists());
        assert_eq!(
            std::fs::read(fixture.path().join(".gitignore")).unwrap(),
            bytes
        );
    }
}

#[test]
fn a_born_head_whose_history_cannot_be_read_is_not_taken_for_an_empty_one() {
    let fixture = fixture();
    let project = fixture.path();
    git(&["init", "-q", "-b", "main"], &project);
    fixture.write("a.txt", "a\n");
    git(&["add", "-A"], &project);
    git(&["commit", "-q", "-m", "first"], &project);
    let head = git(&["rev-parse", "HEAD"], &project).trim().to_owned();
    std::fs::remove_file(
        project
            .join(".git/objects")
            .join(&head[..2])
            .join(&head[2..]),
    )
    .unwrap();
    assert_eq!(fixture.inspect().unwrap_err().0, "github_publish_failed");
}

fn hooks(project: &Path, marks: &Path) {
    for (name, event) in [
        ("index", "post-index-change"),
        ("message", "prepare-commit-msg"),
        ("committed", "post-commit"),
        ("refs", "reference-transaction"),
    ] {
        git(&["config", &format!("hook.{name}.event"), event], project);
        let command = format!("/usr/bin/touch '{}'", marks.join(name).display());
        git(
            &["config", &format!("hook.{name}.command"), &command],
            project,
        );
    }
}

#[test]
fn a_config_hook_in_the_project_never_runs_and_publish_refuses_to_commit() {
    let owned = fixture();
    let project = owned.path();
    git(&["init", "-q", "-b", "main"], &project);
    owned.write("a.txt", "a\n");
    git(&["add", "-A"], &project);
    git(&["commit", "-q", "-m", "first"], &project);
    let head = owned.lines(&["rev-parse", "HEAD"]);
    let marks = tempfile::tempdir().unwrap();
    hooks(&project, marks.path());
    let local = owned.inspect().unwrap();
    assert_eq!(local.history, GitHubHistory::Owner);
    let written = owned.prepare(&local, true, false, &[]).unwrap();
    assert!(
        owned
            .scan(GitHubHistory::Owner)
            .unwrap()
            .findings
            .is_empty()
    );
    let written: Vec<&str> = written.iter().map(String::as_str).collect();
    let refused = owned.commit(&written);
    let ran: Vec<String> = std::fs::read_dir(marks.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(ran.is_empty(), "{ran:?}");
    assert_eq!(refused.unwrap_err().0, "github_publish_hooks_set");
    assert_eq!(owned.lines(&["rev-parse", "HEAD"]), head);
    let fresh = fixture();
    git(&["init", "-q", "-b", "main"], &fresh.path());
    fresh.write("b.txt", "b\n");
    hooks(&fresh.path(), marks.path());
    let local = fresh.inspect().unwrap();
    let written = fresh.prepare(&local, true, false, &[]).unwrap();
    let written: Vec<&str> = written.iter().map(String::as_str).collect();
    let refused = fresh.commit(&written);
    assert!(std::fs::read_dir(marks.path()).unwrap().next().is_none());
    assert_eq!(refused.unwrap_err().0, "github_publish_hooks_set");
}

#[test]
fn a_status_publish_cannot_count_is_refused_before_anything_is_committed() {
    let owned = fixture();
    let project = owned.path();
    git(&["init", "-q", "-b", "main"], &project);
    owned.write("a.txt", "a\n");
    git(&["add", "-A"], &project);
    git(&["commit", "-q", "-m", "first"], &project);
    for index in 0..8 {
        owned.write(&format!("untracked-{index}.txt"), "u\n");
    }
    let head = owned.lines(&["rev-parse", "HEAD"]);
    let local = owned.inspect().unwrap();
    let written = owned.prepare(&local, true, false, &[]).unwrap();
    assert_eq!(written, [".gitignore"]);
    let refused = commit_within(
        Path::new(GIT),
        owned.home.path(),
        &project,
        &written,
        &ada(),
        64,
    );
    assert_eq!(owned.lines(&["rev-parse", "HEAD"]), head);
    assert!(owned.lines(&["diff", "--cached", "--name-only"]).is_empty());
    assert_eq!(refused.unwrap_err().0, "github_publish_too_large");
    let committed = commit_within(
        Path::new(GIT),
        owned.home.path(),
        &project,
        &written,
        &ada(),
        1 << 20,
    )
    .unwrap();
    assert_eq!(committed.left_out, 8);
    assert_eq!(
        owned.lines(&["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]),
        [".gitignore"]
    );
}
