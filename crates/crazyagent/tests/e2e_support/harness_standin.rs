use crazyagent::app_plugins::yaml::Value;
use crazyagent::harnesses::manifest::{self, Manifest};
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The stand-in kinds: the four piped drivers, then the five terminal record readers.
pub const KINDS: [&str; 9] = [
    "acp",
    "jsonl-turn",
    "jsonl-rpc",
    "text-turn",
    "jsonl-append",
    "json-rewrite",
    "sqlite-poll",
    "markdown-history",
    "screen",
];
/// How long a row waits for one line of output, or for a stand-in to exit. It leaves room
/// for the first exec of a newly written program, which macOS may hold in a queue.
pub const WAIT: Duration = Duration::from_secs(30);
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/harness");
const FILE_NAME_BYTES_MAX: usize = 64;
const CHUNK_BYTES: usize = 64 * 1024;
const POLL: Duration = Duration::from_millis(20);

/// One stand-in harness in a private scratch folder: `standin.sh` installed as
/// `bin/standin-<kind>` at mode 0700, the program name its manifest lists, and its state
/// folder `home/standin`, where a row puts fixtures and reads what the stand-in logged.
/// Dropping it removes the folder.
pub struct Standin {
    kind: &'static str,
    folder: tempfile::TempDir,
}

impl Standin {
    /// Installs the stand-in for one kind. Input: a name from `KINDS`. Output: the stand-in,
    /// with its program in place and an empty state folder.
    pub fn new(kind: &str) -> Self {
        let kind = *KINDS
            .iter()
            .find(|known| **known == kind)
            .expect("a stand-in kind");
        let folder = super::scratch("standin-");
        fs::create_dir(folder.path().join("bin")).unwrap();
        fs::create_dir_all(folder.path().join("home").join("standin")).unwrap();
        let standin = Self { kind, folder };
        fs::copy(Path::new(FIXTURES).join("standin.sh"), standin.program()).unwrap();
        fs::set_permissions(standin.program(), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(standin.program().is_file());
        standin
    }

    /// The kind this stand-in plays. Output: its name from `KINDS`.
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// The installed program, named `standin-<kind>`. Output: the path.
    pub fn program(&self) -> PathBuf {
        self.bin().join(format!("standin-{}", self.kind))
    }

    /// The folder that holds the program. It is under `/private/tmp`, and the engine's tool
    /// resolver refuses a temp folder on a search path (`tools.rs`), so a row hands the
    /// program by its full path or installs it outside temp first. Output: the path.
    pub fn bin(&self) -> PathBuf {
        self.folder.path().join("bin")
    }

    /// The home folder the stand-in is handed as `{home}`. Output: the path.
    pub fn home(&self) -> PathBuf {
        self.folder.path().join("home")
    }

    /// The state folder under the home: fixtures go in, logs and default records come
    /// out. Output: the path.
    pub fn state(&self) -> PathBuf {
        self.home().join("standin")
    }

    /// Writes one fixture into the state folder, such as `turn-1.out`. Inputs: a plain file
    /// name and its bytes. Output: none.
    pub fn put(&self, name: &str, bytes: impl AsRef<[u8]>) {
        assert!(plain(name), "{name} is not a plain fixture name");
        fs::write(self.state().join(name), bytes).unwrap();
    }

    /// Reads one file the stand-in wrote into its state folder, such as `prompt-1`.
    /// Input: a plain file name. Output: its text, empty when there is no such file.
    pub fn log(&self, name: &str) -> String {
        assert!(plain(name), "{name} is not a plain log name");
        fs::read_to_string(self.state().join(name)).unwrap_or_default()
    }

    /// The arguments the stand-in got on one run, counted from 1. Input: the run.
    /// Output: the arguments in order.
    pub fn argv(&self, run: usize) -> Vec<String> {
        assert!(run >= 1);
        self.log(&format!("argv-{run}"))
            .split_terminator('\0')
            .map(str::to_owned)
            .collect()
    }

    /// `--home <home>` followed by these arguments, as most rows run the stand-in.
    /// Input: the arguments. Output: the whole argument list.
    pub fn at_home(&self, args: &[&str]) -> Vec<String> {
        let home = self
            .home()
            .to_str()
            .expect("a UTF-8 scratch path")
            .to_owned();
        ["--home".to_owned(), home]
            .into_iter()
            .chain(args.iter().map(|arg| (*arg).to_owned()))
            .collect()
    }

    /// The stand-in's command with exactly these arguments, on a cleared environment of
    /// `HOME` and `PATH` only, run in the scratch folder in a process group of its own.
    /// Input: the arguments. Output: the command.
    pub fn command(&self, argv: &[String]) -> Command {
        let mut command = Command::new(self.program());
        command
            .args(argv)
            .env_clear()
            .env("HOME", self.home())
            .env("PATH", "/usr/bin:/bin")
            .current_dir(self.folder.path())
            .process_group(0);
        command
    }

    /// Starts the stand-in with these arguments and all three pipes held by the row.
    /// Input: the arguments. Output: the running stand-in.
    pub fn start(&self, argv: &[String]) -> Live {
        let mut command = self.command(argv);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = crazyagent::spawn_gate::spawn(&mut command).unwrap();
        Live::new(child)
    }

    /// Runs the stand-in to its end with these arguments and these bytes on its stdin.
    /// Inputs: the arguments and the stdin bytes. Output: how it ended and what it printed.
    pub fn run(&self, argv: &[String], stdin: &[u8]) -> Ended {
        let mut live = self.start(argv);
        live.write(stdin);
        live.finish()
    }
}

/// A running stand-in. Its stdin stays open for the row, its stdout is read on a thread so
/// no row waits on it past `WAIT`, and its stderr is gathered whole. Dropping it kills the
/// stand-in if it still runs.
pub struct Live {
    child: Child,
    stdin: Option<ChildStdin>,
    chunks: mpsc::Receiver<Vec<u8>>,
    buffer: Vec<u8>,
    errors: Option<JoinHandle<Vec<u8>>>,
}

/// How a stand-in ended: its status, everything it printed on each stream, and whether the
/// row killed it because it ran past `WAIT`.
pub struct Ended {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub killed: bool,
}

impl Live {
    fn new(mut child: Child) -> Self {
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("a piped stdout");
        let mut stderr = child.stderr.take().expect("a piped stderr");
        let (sent, chunks) = mpsc::channel();
        std::thread::spawn(move || {
            let mut chunk = vec![0_u8; CHUNK_BYTES];
            while let Ok(read) = stdout.read(&mut chunk) {
                if read == 0 || sent.send(chunk[..read].to_vec()).is_err() {
                    break;
                }
            }
        });
        let errors = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            bytes
        });
        Self {
            child,
            stdin,
            chunks,
            buffer: Vec::new(),
            errors: Some(errors),
        }
    }

    /// The stand-in's process id, which also names its process group. Output: the id.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Writes these bytes to the stand-in's stdin. A stand-in that already ended is not an
    /// error here: the row's next look at its output shows it. Input: the bytes.
    pub fn write(&mut self, bytes: &[u8]) {
        let stdin = self
            .stdin
            .as_mut()
            .expect("the stand-in's stdin is still open");
        match stdin.write_all(bytes).and_then(|()| stdin.flush()) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::BrokenPipe => {}
            Err(error) => panic!("writing to the stand-in failed: {error}"),
        }
    }

    /// Writes one line and its newline to the stand-in's stdin. Input: the line, which
    /// holds no newline of its own.
    pub fn send(&mut self, line: &str) {
        assert!(!line.contains('\n'));
        self.write(format!("{line}\n").as_bytes());
    }

    /// Closes the stand-in's stdin, as a driver does at the end of its input.
    pub fn close(&mut self) {
        self.stdin = None;
    }

    /// The next whole line the stand-in printed, without its newline, waiting up to `WAIT`.
    /// Output: the line, or none when the stand-in ended or `WAIT` passed first.
    pub fn line(&mut self) -> Option<String> {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = self.buffer.drain(..=end).collect();
                return Some(String::from_utf8_lossy(&line[..end]).into_owned());
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            let chunk = self.chunks.recv_timeout(left).ok()?;
            self.buffer.extend_from_slice(&chunk);
        }
    }

    /// Sends a signal to the stand-in's whole process group, as Ctrl-C in a terminal does.
    /// Input: the signal's name for `/bin/kill`, such as `INT`. Output: whether it was sent.
    pub fn signal(&self, name: &str) -> bool {
        crazyagent::spawn_gate::status(
            Command::new("/bin/kill")
                .args(["-s", name, "--", &format!("-{}", self.pid())])
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .is_ok_and(|status| status.success())
    }

    /// Closes stdin and waits up to `WAIT` for the stand-in to exit, killing it past that.
    /// Output: how it ended, with the rest of what it printed.
    pub fn finish(mut self) -> Ended {
        self.close();
        let deadline = Instant::now() + WAIT;
        let mut killed = false;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                killed = true;
                let _ = self.child.kill();
                break self.child.wait().unwrap();
            }
            std::thread::sleep(POLL);
        };
        while let Ok(chunk) = self.chunks.recv_timeout(WAIT) {
            self.buffer.extend_from_slice(&chunk);
        }
        let stderr = self
            .errors
            .take()
            .map(|errors| errors.join().unwrap())
            .unwrap_or_default();
        Ended {
            status,
            stdout: std::mem::take(&mut self.buffer),
            stderr,
            killed,
        }
    }
}

impl Drop for Live {
    /// Kills the stand-in if it still runs, so no row leaves one behind.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// The text of one stand-in manifest. Input: a name from `KINDS`. Output: the YAML text.
pub fn manifest_text(kind: &str) -> String {
    assert!(KINDS.contains(&kind), "{kind} is not a stand-in kind");
    fs::read_to_string(Path::new(FIXTURES).join(format!("standin-{kind}.yaml"))).unwrap()
}

/// One stand-in manifest, read by the engine's manifest reader. Input: a name from
/// `KINDS`. Output: the manifest.
pub fn manifest(kind: &str) -> Manifest {
    manifest::parse(manifest_text(kind).as_bytes()).unwrap()
}

/// One setting of a manifest by its path of keys, such as `["terminal", "record", "path"]`.
/// Inputs: the manifest and the path. Output: the setting as it is written.
pub fn setting<'a>(manifest: &'a Manifest, path: &[&str]) -> &'a Value {
    assert!(!path.is_empty());
    let first = manifest
        .block(path[0])
        .unwrap_or_else(|| panic!("{} is in the manifest", path[0]));
    path[1..].iter().fold(first, |found, key| {
        found
            .get(key)
            .unwrap_or_else(|| panic!("{path:?} is in the manifest"))
    })
}

/// Fills the placeholders of one manifest text as a driver will. Inputs: the text and each
/// placeholder's value. Output: the filled text, with no placeholder left in it.
pub fn fill(text: &str, values: &[(&str, &str)]) -> String {
    let filled = values.iter().fold(text.to_owned(), |text, (name, value)| {
        assert!(manifest::PLACEHOLDERS.contains(name));
        text.replace(&format!("{{{name}}}"), value)
    });
    assert!(
        manifest::PLACEHOLDERS
            .iter()
            .all(|name| !filled.contains(&format!("{{{name}}}"))),
        "{text} kept a placeholder no value filled"
    );
    filled
}

/// Fills one text setting, such as `["terminal", "record", "path"]`. Inputs: the manifest,
/// the setting's path and each placeholder's value. Output: the filled text.
pub fn filled_text(manifest: &Manifest, path: &[&str], values: &[(&str, &str)]) -> String {
    let Value::Str(text) = setting(manifest, path) else {
        panic!("{path:?} is text in the stand-in manifest");
    };
    fill(text, values)
}

/// Fills one argument list setting, such as `["piped", "argv"]`. Inputs: the manifest, the
/// setting's path and each placeholder's value. Output: the filled arguments.
pub fn filled_argv(manifest: &Manifest, path: &[&str], values: &[(&str, &str)]) -> Vec<String> {
    let Value::List(items) = setting(manifest, path) else {
        panic!("{path:?} is a list in the stand-in manifest");
    };
    items
        .iter()
        .map(|item| {
            let Value::Str(text) = item else {
                panic!("every entry of {path:?} is text");
            };
            fill(text, values)
        })
        .collect()
}

fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= FILE_NAME_BYTES_MAX
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
