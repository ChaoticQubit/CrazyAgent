#![forbid(unsafe_code)]

use crate::filter::{self, Call, Calls, Guards};
use crate::rules::{self, Dest, GitOwn, Reach, Target};
use crate::sys::{self, Notice};
use rustix::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, RenameFlags};
use rustix::io::Errno;
use rustix::net::SocketAddrUnix;
use rustix::process::{Pid, PidfdFlags, PidfdGetfdFlags, Signal, WaitOptions};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::FileExt;
use std::os::unix::net::UnixStream;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

pub const WORKERS: usize = 8;
pub const STOPPED_EXIT: i32 = 125;
const PATH_BYTES_MAX: usize = 4096;
const LINK_HOPS_MAX: usize = 40;
const AT_FDCWD: i32 = -100;
const F_OWNER_PID: i32 = 1;
const F_OWNER_PGRP: i32 = 2;
const CREAT_FLAGS: u64 = 0o1101;

/// What one agent's supervisor holds: the ports it may reach and where, when its connects
/// are guarded; its own git folders, when new entries are guarded; and whether its
/// signals stay inside the programs it started.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Supervised {
    pub reach: Option<Reach>,
    pub git: Option<GitOwn>,
    pub signals: bool,
}

impl Supervised {
    /// The guards the harness's filter carries for this agent. Output: one per rule set.
    pub fn guards(&self) -> Guards {
        Guards {
            connect: self.reach.is_some(),
            create: self.git.is_some(),
            signal: self.signals,
        }
    }
}

/// Starts the harness with its guarded calls trapped and answers them until it ends. The
/// calling thread must be the one that waits here: the harness is killed when it ends. A
/// worker that stops ends the supervisor, so no trapped call is left without an answer.
/// Inputs: what to hold and the harness command. Output: the harness's exit code, 128
/// plus the signal that ended it, or the reason no harness was supervised.
pub fn run(config: Supervised, command: &mut Command) -> io::Result<i32> {
    let guards = config.guards();
    assert!(guards != Guards::default());
    let calls = Calls::host().ok_or(io::ErrorKind::Unsupported)?;
    sys::sizes_fit()?;
    let program = sys::Program::new(&filter::program(&calls, guards));
    let own = rustix::process::getpid();
    rustix::process::set_child_subreaper(Some(own))?;
    let (ours, theirs) = UnixStream::pair()?;
    let harness = sys::spawn_trapped(command, program, theirs.into())?.id();
    let listener = sys::take_listener(ours.as_fd())?;
    rustix::process::umask(Mode::empty());
    let own = own.as_raw_nonzero().get();
    let server = Arc::new(Server {
        config,
        calls,
        own,
        leads: place_of(own).is_some_and(|stat| stat.group == own),
        listener,
    });
    for _ in 0..WORKERS {
        let server = Arc::clone(&server);
        let worker = move || {
            std::panic::catch_unwind(AssertUnwindSafe(|| server.serve())).ok();
            std::process::exit(STOPPED_EXIT)
        };
        std::thread::Builder::new().spawn(worker)?;
    }
    ended(harness)
}

/// Waits until the harness ends, reaping every orphan this process adopted meanwhile.
/// Input: the harness's process id. Output: its exit code, or 128 plus its last signal.
fn ended(harness: u32) -> io::Result<i32> {
    assert!(harness > 0);
    loop {
        match rustix::process::wait(WaitOptions::empty()) {
            Ok(Some((pid, status))) if pid.as_raw_nonzero().get().unsigned_abs() == harness => {
                let signalled = status.terminating_signal().map(|signal| 128 + signal);
                return Ok(status.exit_status().or(signalled).unwrap_or(1));
            }
            Ok(_) | Err(Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

/// Turns an I/O error into the error number a trapped call is answered with.
fn errno(error: io::Error) -> Errno {
    Errno::from_io_error(&error).unwrap_or(Errno::IO)
}

/// Tells whether a listener error only means the caller is gone or the wait was cut.
fn passing(error: &io::Error) -> bool {
    matches!(
        Errno::from_io_error(error),
        Some(Errno::INTR | Errno::NOENT)
    )
}

/// Reads a process's parent, group and session from `/proc`. Input: a process or thread
/// id. Output: the fields, or none when it is gone.
fn place_of(pid: i32) -> Option<rules::Stat> {
    let text = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    rules::stat(&String::from_utf8_lossy(&text))
}

/// Reads a process's parent from `/proc`, for [`rules::in_tree`].
fn parent_of(pid: i32) -> Option<i32> {
    place_of(pid).map(|stat| stat.parent)
}

/// The real path of a folder this process holds open. Input: the descriptor.
fn real_of(held: BorrowedFd<'_>) -> Result<PathBuf, Errno> {
    std::fs::read_link(format!("/proc/self/fd/{}", held.as_raw_fd())).map_err(errno)
}

/// Tells whether a name is there, a link itself and not what it points at.
fn exists(place: &Place) -> bool {
    rustix::fs::statat(&place.folder, place.name(), AtFlags::SYMLINK_NOFOLLOW).is_ok()
}

/// The kind of entry `mknod` makes for a mode: a regular file when the mode names none.
fn node_type(mode: u32) -> FileType {
    match FileType::from_raw_mode(mode) {
        FileType::Unknown => FileType::RegularFile,
        kind => kind,
    }
}

/// Logs an entry the supervisor made and could not take back after a filesystem read its
/// name as `.git`. Input: the result of taking it back.
fn undone(taken_back: Result<(), Errno>) {
    if let Err(error) = taken_back {
        eprintln!("crazyagent-landlock: an entry read as .git was not removed: {error}");
    }
}

/// What a trapped call is answered with: a return value, leave to run as the caller made
/// it, or nothing more because the answer went out with a descriptor.
enum Answer {
    Value(i64),
    Continue,
    Handed,
}

/// Where a relative path starts: the caller's working folder, one of its descriptors by
/// number, or a folder the supervisor already holds (a link's own folder).
enum Base {
    Here,
    Number(i32),
    Held(OwnedFd),
}

impl Base {
    /// Reads a folder-descriptor register as the kernel does, from its low 32 bits.
    fn of(register: u64) -> Self {
        match register as u32 as i32 {
            AT_FDCWD => Self::Here,
            number => Self::Number(number),
        }
    }
}

/// The folder a new entry goes into, held open, with the entry's name, the folder's real
/// path and whether the folder is on procfs, where nothing new can be made.
struct Place {
    folder: OwnedFd,
    name: OsString,
    real: PathBuf,
    proc: bool,
}

impl Place {
    /// The entry's name, one path component.
    fn name(&self) -> &OsStr {
        self.name.as_os_str()
    }

    /// The real path the entry has or would have.
    fn entry(&self) -> PathBuf {
        self.real.join(&self.name)
    }
}

/// What a single-path creating call makes: a folder with a mode, a node with a mode and
/// a device, or a link with its target text.
enum Make {
    Folder(u64),
    Node(u64, u64),
    Link(Vec<u8>),
}

/// What a two-path creating call does, with its flags register: a hard link or a rename.
enum Pair {
    Link(u64),
    Rename(u64),
}

/// The thread that made a trapped call: its ids, its file mode mask and its memory.
struct Caller {
    tid: i32,
    tgid: i32,
    umask: u32,
    memory: File,
}

impl Caller {
    /// Reads who made a call. Input: the thread id from the notification. Output: the
    /// caller, or none when it is gone or its memory may not be read.
    fn read(tid: i32) -> Option<Self> {
        let status = std::fs::read(format!("/proc/{tid}/status")).ok()?;
        let status = String::from_utf8_lossy(&status);
        let tgid: i32 = rules::field(&status, "Tgid")?.parse().ok()?;
        let umask = rules::umask(&status)?;
        let memory = File::open(format!("/proc/{tid}/mem")).ok()?;
        (tid > 0 && tgid > 0).then_some(Self {
            tid,
            tgid,
            umask,
            memory,
        })
    }

    /// Copies bytes from the caller's memory. Inputs: the address and the count. Output:
    /// exactly that many bytes, or `EFAULT`.
    fn bytes(&self, at: u64, len: usize) -> Result<Vec<u8>, Errno> {
        assert!(len <= PATH_BYTES_MAX);
        let mut bytes = vec![0u8; len];
        let read = self.memory.read_exact_at(&mut bytes, at);
        read.map(|()| bytes).map_err(|_| Errno::FAULT)
    }

    /// Copies a zero-ended text from the caller's memory. Input: the address. Output: the
    /// bytes before the zero; `EFAULT` when it cannot be read, `ENAMETOOLONG` when no
    /// zero comes within a path's length.
    fn text(&self, at: u64) -> Result<Vec<u8>, Errno> {
        let mut text = vec![0u8; PATH_BYTES_MAX];
        let mut filled = 0;
        while filled < text.len() {
            let from = at.checked_add(filled as u64).ok_or(Errno::FAULT)?;
            let read = self.memory.read_at(&mut text[filled..], from);
            let read = read.ok().filter(|read| *read > 0).ok_or(Errno::FAULT)?;
            if let Some(end) = text[filled..filled + read].iter().position(|byte| *byte == 0) {
                text.truncate(filled + end);
                return Ok(text);
            }
            filled += read;
        }
        Err(Errno::NAMETOOLONG)
    }
}

/// One agent's supervisor: what it holds, the architecture's call numbers, its own
/// process id, whether it leads its process group, and the listener its workers share.
struct Server {
    config: Supervised,
    calls: Calls,
    own: i32,
    leads: bool,
    listener: OwnedFd,
}

impl Server {
    /// Answers trapped calls one after another for as long as the process lives. A
    /// listener that fails ends the whole supervisor, and with it the harness.
    fn serve(&self) {
        let listener = self.listener.as_fd();
        loop {
            let notice = match sys::receive(listener) {
                Ok(notice) => notice,
                Err(error) if passing(&error) => continue,
                Err(error) => {
                    eprintln!("crazyagent-landlock: the call supervisor stopped: {error}");
                    std::process::exit(STOPPED_EXIT);
                }
            };
            let sent = match self.answer(&notice) {
                Ok(Answer::Value(value)) => sys::respond(listener, notice.id, Ok(value)),
                Ok(Answer::Continue) => sys::respond_continue(listener, notice.id),
                Ok(Answer::Handed) => Ok(()),
                Err(errno) => sys::respond(listener, notice.id, Err(errno.raw_os_error())),
            };
            if let Err(error) = sent
                && !passing(&error)
            {
                eprintln!("crazyagent-landlock: a trapped call got no answer: {error}");
            }
        }
    }

    /// Decides one trapped call. Input: the call. Output: the answer, or the error number
    /// the caller gets; a call the filter never traps is answered as missing.
    fn answer(&self, notice: &Notice) -> Result<Answer, Errno> {
        let number = u32::try_from(notice.nr).map_err(|_| Errno::NOSYS)?;
        let known = self.calls.call(number);
        let call = known.filter(|_| notice.arch == self.calls.arch);
        let call = call.ok_or(Errno::NOSYS)?;
        let caller = Caller::read(notice.tid).ok_or(Errno::SRCH)?;
        let job = Job {
            server: self,
            notice,
            caller: &caller,
        };
        match call {
            Call::Connect => job.connect(),
            Call::Fcntl | Call::Ioctl => job.owner(call == Call::Fcntl),
            call if filter::SIGNALLING.contains(&call) => job.signal(call),
            call => job.create(call),
        }
    }
}

/// One trapped call being answered: the supervisor, the call and who made it.
struct Job<'a> {
    server: &'a Server,
    notice: &'a Notice,
    caller: &'a Caller,
}

impl Job<'_> {
    /// Tells whether the call still waits. Asked after the caller's memory was read and
    /// after its process or folder is held here, so what was read and what is held are
    /// the caller's and not those of a process that took its id since.
    fn alive(&self) -> Result<(), Errno> {
        let listener = self.server.listener.as_fd();
        let waits = sys::still_valid(listener, self.notice.id);
        waits.then_some(()).ok_or(Errno::NOENT)
    }

    /// Takes a copy of one of the caller's descriptors, so what the supervisor acts on
    /// cannot be swapped under it. The call must still wait once the caller's process is
    /// held. Input: the descriptor register. Output: the copy.
    fn taken(&self, register: u64) -> Result<OwnedFd, Errno> {
        let number = i32::try_from(register as u32).map_err(|_| Errno::BADF)?;
        let process = Pid::from_raw(self.caller.tgid).ok_or(Errno::SRCH)?;
        let held = rustix::process::pidfd_open(process, PidfdFlags::empty())?;
        self.alive()?;
        rustix::process::pidfd_getfd(&held, number, PidfdGetfdFlags::empty())
    }

    /// Connects for the caller: the address is copied, judged against the ports the agent
    /// may reach, and used on a copy of the caller's socket, so neither can change after
    /// the check. Output: zero, or the connect's own error; `EACCES` when refused.
    fn connect(&self) -> Result<Answer, Errno> {
        let reach = self.server.config.reach.as_ref().ok_or(Errno::NOSYS)?;
        let [socket, at, len, ..] = self.notice.args;
        let len = len as u32 as usize;
        if len > rules::ADDRESS_BYTES_MAX {
            return Err(Errno::INVAL);
        }
        let bytes = self.caller.bytes(at, len)?;
        let socket = self.taken(socket)?;
        let connected = match rules::dest(&bytes) {
            Dest::Ip(_) if !reach.allows(&rules::address(&bytes)) => Err(Errno::ACCESS),
            Dest::Ip(to) => rustix::net::connect(&socket, &to),
            Dest::UnixName(name) => {
                rustix::net::connect(&socket, &SocketAddrUnix::new_abstract_name(&name)?)
            }
            Dest::UnixPath(path) if path.is_empty() => Err(Errno::INVAL),
            Dest::UnixPath(path) => {
                rustix::net::connect(&socket, &SocketAddrUnix::new(self.unix_path(&path))?)
            }
            Dest::Unspec => rustix::net::connect_unspec(&socket),
            Dest::Other => Err(Errno::AFNOSUPPORT),
        };
        connected.map(|()| Answer::Value(0))
    }

    /// The path a Unix socket address names, as this process must spell it: a relative
    /// one starts at the caller's working folder. Input: the path bytes.
    fn unix_path(&self, path: &[u8]) -> PathBuf {
        let path = Path::new(OsStr::from_bytes(path));
        if path.is_absolute() {
            return path.to_path_buf();
        }
        PathBuf::from(format!("/proc/{}/cwd", self.caller.tgid)).join(path)
    }

    /// Tells whether a signal's target is inside the agent: a program it started, the
    /// caller itself, or a process group led by one of them or by the supervisor when it
    /// leads the agent's own group. Input: the target. Output: true to let it through.
    fn inside(&self, target: Target) -> bool {
        let (own, caller) = (self.server.own, self.caller);
        let tree = |pid: i32| {
            pid == caller.tgid || pid == caller.tid || rules::in_tree(pid, own, parent_of)
        };
        let group = |group: i32| (self.server.leads && group == own) || tree(group);
        match target {
            Target::One(pid) => tree(pid),
            Target::Group(id) => group(id),
            Target::OwnGroup => place_of(caller.tid).is_some_and(|stat| group(stat.group)),
            Target::Everyone => false,
        }
    }

    /// Judges a signal call by its register arguments and lets the kernel run it when its
    /// target is inside the agent. Input: which call. Output: continue, or `EPERM`.
    fn signal(&self, call: Call) -> Result<Answer, Errno> {
        let first = self.notice.args[0] as i64;
        let target = match call {
            Call::PidfdSendSignal => return self.signal_held(),
            Call::Kill => rules::signal_target(first),
            _ if first as i32 > 0 => Target::One(first as i32),
            _ => Target::Everyone,
        };
        let inside = self.inside(target);
        inside.then_some(Answer::Continue).ok_or(Errno::PERM)
    }

    /// Sends a signal through a process descriptor for the caller: the descriptor is
    /// copied, the process it names is judged, and the signal goes out from here. A
    /// signal with made-up details or flags is refused. Output: zero, or the error.
    fn signal_held(&self) -> Result<Answer, Errno> {
        let [held, signal, details, flags, ..] = self.notice.args;
        if details != 0 {
            return Err(Errno::PERM);
        }
        if flags as u32 != 0 {
            return Err(Errno::INVAL);
        }
        let held = self.taken(held)?;
        let about = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", held.as_raw_fd()));
        let about = about.map_err(|_| Errno::BADF)?;
        let pid = rules::field(&about, "Pid").and_then(|pid| pid.parse::<i32>().ok());
        let pid = pid.ok_or(Errno::BADF)?;
        if pid <= 0 {
            return Err(Errno::SRCH);
        }
        if !self.inside(Target::One(pid)) {
            return Err(Errno::PERM);
        }
        if signal as i32 != 0 {
            let signal = Signal::from_named_raw(signal as i32).ok_or(Errno::INVAL)?;
            rustix::process::pidfd_send_signal(&held, signal)?;
        }
        Ok(Answer::Value(0))
    }

    /// Judges who a descriptor's signals are sent to. `F_SETOWN` names the owner in a
    /// register and runs as made; the forms that name it in memory are copied, judged and
    /// set from here on a copy of the descriptor. Input: whether the call is `fcntl`.
    fn owner(&self, fcntl: bool) -> Result<Answer, Errno> {
        let [file, command, value, ..] = self.notice.args;
        if fcntl && command as u32 == filter::F_SETOWN {
            let target = rules::owner_target(value as i64);
            let inside = target.is_none_or(|target| self.inside(target));
            return inside.then_some(Answer::Continue).ok_or(Errno::PERM);
        }
        let bytes = self.caller.bytes(value, if fcntl { 8 } else { 4 })?;
        let word = |at: usize| {
            let word = bytes.get(at..)?.first_chunk::<4>()?;
            Some(i32::from_ne_bytes(*word)).filter(|id| !fcntl || *id >= 0)
        };
        let owner = match (fcntl, word(0)) {
            (false, Some(owner)) => owner,
            (true, Some(F_OWNER_PID)) => word(4).ok_or(Errno::INVAL)?,
            (true, Some(F_OWNER_PGRP)) => -word(4).ok_or(Errno::INVAL)?,
            _ => return Err(Errno::INVAL),
        };
        let target = rules::owner_target(i64::from(owner));
        if target.is_some_and(|target| !self.inside(target)) {
            return Err(Errno::PERM);
        }
        let held = self.taken(file)?;
        sys::set_owner(held.as_fd(), owner).map_err(errno)?;
        Ok(Answer::Value(0))
    }

    /// Reads a creating call's paths from the caller's memory and makes the entry for it.
    /// Input: which call. Output: the call's own result, or `EACCES` for a refused entry.
    fn create(&self, call: Call) -> Result<Answer, Errno> {
        let a = self.notice.args;
        let here = AT_FDCWD as u32 as u64;
        let text = |at: u64| self.caller.text(at);
        match call {
            Call::Open => self.open(here, text(a[0])?, a[1], a[2]),
            Call::Creat => self.open(here, text(a[0])?, CREAT_FLAGS, a[1]),
            Call::Openat => self.open(a[0], text(a[1])?, a[2], a[3]),
            Call::Mkdir => self.make(here, text(a[0])?, Make::Folder(a[1])),
            Call::Mkdirat => self.make(a[0], text(a[1])?, Make::Folder(a[2])),
            Call::Mknod => self.make(here, text(a[0])?, Make::Node(a[1], a[2])),
            Call::Mknodat => self.make(a[0], text(a[1])?, Make::Node(a[2], a[3])),
            Call::Symlink => self.make(here, text(a[1])?, Make::Link(text(a[0])?)),
            Call::Symlinkat => self.make(a[1], text(a[2])?, Make::Link(text(a[0])?)),
            Call::Link => self.pair((here, text(a[0])?), (here, text(a[1])?), Pair::Link(0)),
            Call::Linkat => self.pair((a[0], text(a[1])?), (a[2], text(a[3])?), Pair::Link(a[4])),
            Call::Rename => self.pair((here, text(a[0])?), (here, text(a[1])?), Pair::Rename(0)),
            Call::Renameat => self.pair((a[0], text(a[1])?), (a[2], text(a[3])?), Pair::Rename(0)),
            Call::Renameat2 => {
                self.pair((a[0], text(a[1])?), (a[2], text(a[3])?), Pair::Rename(a[4]))
            }
            _ => Err(Errno::NOSYS),
        }
    }

    /// Resolves the folder a path's last name lies in, as the caller would: from its root,
    /// its working folder or its descriptor, with `/proc/self` read as the caller. The
    /// call must still wait once the folder is held. Inputs: where a relative path starts
    /// and the path. Output: the place; `EEXIST` when the path names no entry of its own
    /// (the root, `.` or `..`).
    fn place(&self, base: &Base, path: &[u8]) -> Result<Place, Errno> {
        if path.is_empty() {
            return Err(Errno::NOENT);
        }
        let (tgid, tid) = (self.caller.tgid, self.caller.tid);
        let path = rules::as_agent(path, tgid, tid);
        let (folder, name) = rules::split_last(&path).ok_or(Errno::EXIST)?;
        if name == b"." || name == b".." {
            return Err(Errno::EXIST);
        }
        let open = OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let from = |start: String| {
            OsString::from_vec([start.as_bytes(), &b"/"[..], folder].concat())
        };
        let start = match base {
            _ if folder.first() == Some(&b'/') => Ok(from(format!("/proc/{tgid}/root"))),
            Base::Here => Ok(from(format!("/proc/{tgid}/cwd"))),
            Base::Number(number) if *number >= 0 => Ok(from(format!("/proc/{tgid}/fd/{number}"))),
            Base::Number(_) => return Err(Errno::BADF),
            Base::Held(held) if folder.is_empty() => Err((held, OsStr::new("."))),
            Base::Held(held) => Err((held, OsStr::from_bytes(folder))),
        };
        let mut held = match start {
            Ok(path) => rustix::fs::openat(CWD, path, open, Mode::empty())?,
            Err((held, path)) => rustix::fs::openat(held, path, open, Mode::empty())?,
        };
        let mut real = real_of(held.as_fd())?;
        let own = (self.server.own, rustix::thread::gettid().as_raw_nonzero().get());
        if let Some(agent) = rules::from_own(&real, own, (tgid, tid)) {
            held = rustix::fs::openat(CWD, agent.as_path(), open, Mode::empty())?;
            real = agent;
        }
        assert!(!name.is_empty() && !name.contains(&b'/'));
        self.alive()?;
        Ok(Place {
            proc: rustix::fs::fstatfs(&held)?.f_type == rustix::fs::PROC_SUPER_MAGIC,
            folder: held,
            name: OsString::from_vec(name.to_vec()),
            real,
        })
    }

    /// Refuses a new entry whose path holds a `.git` that is not one of the agent's own.
    fn judged(&self, place: &Place) -> Result<(), Errno> {
        let git = self.server.config.git.as_ref().ok_or(Errno::NOSYS)?;
        let allowed = place.proc || git.allows(&place.entry());
        allowed.then_some(()).ok_or(Errno::ACCESS)
    }

    /// Refuses to move away a folder that holds one of the agent's own git folders.
    fn moved(&self, place: &Place) -> Result<(), Errno> {
        let git = self.server.config.git.as_ref().ok_or(Errno::NOSYS)?;
        let allowed = place.proc || git.moves(&place.entry());
        allowed.then_some(()).ok_or(Errno::ACCESS)
    }

    /// Tells whether the filesystem reads an entry just made as `.git` although its name
    /// is spelled otherwise: the folder's `.git` then is that same entry.
    fn aliases(&self, place: &Place) -> bool {
        if place.proc || place.name() == ".git" {
            return false;
        }
        let seen = |name: &OsStr| {
            let stat = rustix::fs::statat(&place.folder, name, AtFlags::SYMLINK_NOFOLLOW);
            stat.ok().map(|stat| (stat.st_dev, stat.st_ino))
        };
        let made = seen(place.name());
        made.is_some() && made == seen(OsStr::new(".git"))
    }

    /// Answers an open with the file opened here, as a descriptor of the caller's own.
    fn hand(&self, file: OwnedFd, asked: OFlags) -> Result<Answer, Errno> {
        let listener = self.server.listener.as_fd();
        let cloexec = asked.contains(OFlags::CLOEXEC);
        let handed = sys::hand_descriptor(listener, self.notice.id, file.as_fd(), cloexec);
        handed.map(|()| Answer::Handed).map_err(errno)
    }

    /// Opens a file that may be new for the caller: the last name is opened in its
    /// resolved folder without following a link, and a link there is followed by hand so
    /// every hop is judged. On procfs, and for a path-only open, nothing new can be made,
    /// so the open runs as asked. Inputs: the folder register, the path, flags and mode.
    fn open(&self, base: u64, path: Vec<u8>, flags: u64, mode: u64) -> Result<Answer, Errno> {
        let asked = OFlags::from_bits_retain(flags as u32);
        if path.ends_with(b"/") && !asked.contains(OFlags::PATH) {
            return Err(Errno::ISDIR);
        }
        let mode = Mode::from_bits_truncate(rules::mode_under(mode as u32, self.caller.umask));
        let follows = !asked.intersects(OFlags::NOFOLLOW | OFlags::EXCL);
        let (mut base, mut path) = (Base::of(base), path);
        for _ in 0..=LINK_HOPS_MAX {
            let found = self.place(&base, &path);
            let place = found.map_err(|errno| match errno {
                Errno::EXIST => Errno::ISDIR,
                errno => errno,
            })?;
            let (folder, name) = (&place.folder, place.name());
            if place.proc || asked.contains(OFlags::PATH) {
                let plain = asked | OFlags::CLOEXEC;
                return self.hand(rustix::fs::openat(folder, name, plain, mode)?, asked);
            }
            self.judged(&place)?;
            let fresh = !exists(&place);
            let kept = asked | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            match rustix::fs::openat(folder, name, kept, mode) {
                Ok(_) if fresh && self.aliases(&place) => {
                    undone(rustix::fs::unlinkat(folder, name, AtFlags::empty()));
                    return Err(Errno::ACCESS);
                }
                Ok(file) => return self.hand(file, asked),
                Err(Errno::LOOP) if follows => {
                    path = rustix::fs::readlinkat(folder, name, Vec::new())?.into_bytes();
                    base = Base::Held(place.folder);
                }
                Err(errno) => return Err(errno),
            }
        }
        Err(Errno::LOOP)
    }

    /// Makes a folder, a node or a link for the caller in the resolved folder, with the
    /// caller's file mode mask. Inputs: the folder register, the path and what to make.
    fn make(&self, base: u64, path: Vec<u8>, make: Make) -> Result<Answer, Errno> {
        let place = self.place(&Base::of(base), &path)?;
        self.judged(&place)?;
        let umask = self.caller.umask;
        let mode = |raw: u64| Mode::from_bits_truncate(rules::mode_under(raw as u32, umask));
        let (folder, name) = (&place.folder, place.name());
        let made = match &make {
            Make::Folder(raw) => rustix::fs::mkdirat(folder, name, mode(*raw)),
            Make::Node(raw, device) => {
                rustix::fs::mknodat(folder, name, node_type(*raw as u32), mode(*raw), *device)
            }
            Make::Link(target) => rustix::fs::symlinkat(OsStr::from_bytes(target), folder, name),
        };
        made?;
        if self.aliases(&place) {
            let how = match make {
                Make::Folder(_) => AtFlags::REMOVEDIR,
                _ => AtFlags::empty(),
            };
            undone(rustix::fs::unlinkat(folder, name, how));
            return Err(Errno::ACCESS);
        }
        Ok(Answer::Value(0))
    }

    /// Links or renames for the caller between two resolved folders. The new name is
    /// judged as a new entry, and a rename also by what it moves away. Inputs: each
    /// path with its folder register, and which call with its flags.
    fn pair(&self, old: (u64, Vec<u8>), new: (u64, Vec<u8>), pair: Pair) -> Result<Answer, Errno> {
        let from = self.place(&Base::of(old.0), &old.1)?;
        let to = self.place(&Base::of(new.0), &new.1)?;
        self.judged(&to)?;
        let fresh = !exists(&to);
        let (a, b) = ((&from.folder, from.name()), (&to.folder, to.name()));
        match pair {
            Pair::Link(flags) => {
                let follow = AtFlags::from_bits_retain(flags as u32) & AtFlags::SYMLINK_FOLLOW;
                rustix::fs::linkat(a.0, a.1, b.0, b.1, follow)?;
                if fresh && self.aliases(&to) {
                    undone(rustix::fs::unlinkat(b.0, b.1, AtFlags::empty()));
                    return Err(Errno::ACCESS);
                }
            }
            Pair::Rename(flags) => {
                let flags = RenameFlags::from_bits_retain(flags as u32);
                self.moved(&from)?;
                if flags.contains(RenameFlags::EXCHANGE) {
                    self.judged(&from)?;
                    self.moved(&to)?;
                }
                rustix::fs::renameat_with(a.0, a.1, b.0, b.1, flags)?;
                if fresh && self.aliases(&to) {
                    let back = RenameFlags::NOREPLACE;
                    undone(rustix::fs::renameat_with(b.0, b.1, a.0, a.1, back));
                    return Err(Errno::ACCESS);
                }
            }
        }
        Ok(Answer::Value(0))
    }
}
