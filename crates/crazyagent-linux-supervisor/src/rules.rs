#![forbid(unsafe_code)]

use std::ffi::OsStr;
use std::net::Ipv6Addr;
use std::path::{Component, Path, PathBuf};

pub const AF_INET: u16 = 2;
pub const AF_INET6: u16 = 10;
pub const ADDRESS_BYTES_MAX: usize = 128;
pub const TREE_DEPTH_MAX: usize = 256;

/// Where a connect goes, as read from the bytes the agent handed the kernel: an IPv4 or
/// IPv6 address with its port, or anything else, which no rule lets through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Address {
    V4 { ip: [u8; 4], port: u16 },
    V6 { ip: [u8; 16], port: u16 },
    Other,
}

/// Reads a socket address the way the kernel does: the family in the machine's byte
/// order, the port in network order. Input: the copied bytes, at most
/// [`ADDRESS_BYTES_MAX`]. Output: the address; `Other` for a family that is not IPv4 or
/// IPv6 and for bytes too short to hold the address.
pub fn address(bytes: &[u8]) -> Address {
    assert!(bytes.len() <= ADDRESS_BYTES_MAX);
    let Some(family) = bytes.first_chunk::<2>() else {
        return Address::Other;
    };
    let port = |bytes: &[u8]| u16::from_be_bytes([bytes[2], bytes[3]]);
    match u16::from_ne_bytes(*family) {
        AF_INET if bytes.len() >= 8 => Address::V4 {
            ip: [bytes[4], bytes[5], bytes[6], bytes[7]],
            port: port(bytes),
        },
        AF_INET6 if bytes.len() >= 24 => {
            let mut ip = [0u8; 16];
            ip.copy_from_slice(&bytes[8..24]);
            Address::V6 {
                ip,
                port: port(bytes),
            }
        }
        _ => Address::Other,
    }
}

/// The TCP ports an agent may reach: `any` on every host, `loopback` only on this
/// computer's loopback addresses (127.0.0.0/8, `::1` and their IPv4-mapped form).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reach {
    pub any: Vec<u16>,
    pub loopback: Vec<u16>,
}

impl Reach {
    /// Tells whether a connect may go out. A port allowed on every host wins over the
    /// same port allowed on loopback. Input: the address. Output: true to connect on the
    /// agent's behalf, false to refuse.
    pub fn allows(&self, address: &Address) -> bool {
        let (port, local) = match address {
            Address::V4 { ip, port } => (*port, ip[0] == 127),
            Address::V6 { ip, port } => (*port, local_v6(ip)),
            Address::Other => return false,
        };
        self.any.contains(&port) || (local && self.loopback.contains(&port))
    }
}

/// Tells whether an IPv6 address is this computer's loopback: `::1`, or an IPv4-mapped
/// address in 127.0.0.0/8. Input: the sixteen bytes. Output: true for loopback.
fn local_v6(ip: &[u8; 16]) -> bool {
    let mapped = ip[..10].iter().all(|byte| *byte == 0) && ip[10..12] == [0xff, 0xff];
    *ip == Ipv6Addr::LOCALHOST.octets() || (mapped && ip[12] == 127)
}

/// Tells whether a name is `.git` in any letter case. Input: one path component.
pub fn is_git(name: &OsStr) -> bool {
    name.as_encoded_bytes().eq_ignore_ascii_case(b".git")
}

/// The git folders an agent may make entries in: the top-level `.git` of each working
/// tree in `trees`, the `.git` of each folder directly inside a folder in `kids`, and the
/// `exact` paths. Every other entry named `.git`, and everything below one, is refused.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GitOwn {
    pub trees: Vec<PathBuf>,
    pub kids: Vec<PathBuf>,
    pub exact: Vec<PathBuf>,
}

impl GitOwn {
    /// Tells whether a new entry may be made at a path. Input: the real, absolute path
    /// of the entry, its folder resolved and no `.` or `..` in it. Output: true when no
    /// component is a `.git` in any letter case, or the first one is one of the agent's
    /// own; false otherwise and for a path that is not in that form.
    pub fn allows(&self, real: &Path) -> bool {
        let mut walked = PathBuf::new();
        for part in real.components() {
            match part {
                Component::RootDir if walked.as_os_str().is_empty() => walked.push("/"),
                Component::Normal(name) if walked.is_absolute() => {
                    walked.push(name);
                    if is_git(name) {
                        return self.owns(&walked);
                    }
                }
                _ => return false,
            }
        }
        walked.is_absolute()
    }

    /// Tells whether a folder or file may be moved away from a path: never a folder that
    /// holds one of the agent's own `.git` entries, nor a folder above one, since the move
    /// would land that `.git` where none may be made. Input: the real, absolute source
    /// path. Output: true when no own git folder sits at or below it.
    pub fn moves(&self, real: &Path) -> bool {
        let holds = |folder: &Path| folder.starts_with(real);
        let kid = |kid: &PathBuf| holds(kid) || real.parent() == Some(kid.as_path());
        let exact = |own: &PathBuf| own.parent().is_some_and(holds);
        real.is_absolute()
            && !self.trees.iter().any(|tree| holds(tree))
            && !self.kids.iter().any(kid)
            && !self.exact.iter().any(exact)
    }

    /// Tells whether a path ending in a `.git` component is one of the agent's own git
    /// folders, named exactly `.git`. Input: the path up to that component. Output: true
    /// for a tree's top-level one, a kid folder's one, or an exact one.
    fn owns(&self, git: &Path) -> bool {
        let Some(folder) = git.parent() else {
            return false;
        };
        let plain = git.file_name() == Some(OsStr::new(".git"));
        let tree = self.trees.iter().any(|tree| tree == folder);
        let above = folder.parent();
        let kid = above.is_some_and(|above| self.kids.iter().any(|kid| kid == above));
        plain && (tree || kid || self.exact.iter().any(|own| own == git))
    }
}

/// Whom a signal call names: one process, one process group, the caller's own group, or
/// every process the caller may signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    One(i32),
    Group(i32),
    OwnGroup,
    Everyone,
}

/// Reads the pid register of `kill(2)` as the kernel does, from its low 32 bits. Input:
/// the register. Output: the target; a group id that cannot be negated is `Everyone`,
/// which is always refused.
pub fn signal_target(pid: i64) -> Target {
    match pid as i32 {
        0 => Target::OwnGroup,
        -1 => Target::Everyone,
        pid if pid > 0 => Target::One(pid),
        pid => pid.checked_neg().map_or(Target::Everyone, Target::Group),
    }
}

/// Reads the owner a descriptor's signals are sent to (`F_SETOWN`, `FIOSETOWN`): a
/// positive value names a process, a negative one a process group. Input: the value.
/// Output: the target, or none for zero, which names no one.
pub fn owner_target(owner: i64) -> Option<Target> {
    match owner as i32 {
        0 => None,
        pid if pid > 0 => Some(Target::One(pid)),
        pid => Some(pid.checked_neg().map_or(Target::Everyone, Target::Group)),
    }
}

/// Tells whether a process was started by the agent: its chain of parents reaches the
/// supervisor, which adopts the agent's orphans. The supervisor itself is not inside.
/// Inputs: the process, the supervisor's pid, and a reader of a process's parent. Output:
/// true within [`TREE_DEPTH_MAX`] steps, false otherwise.
pub fn in_tree(target: i32, root: i32, parent_of: impl Fn(i32) -> Option<i32>) -> bool {
    assert!(root > 0);
    let mut pid = target;
    for _ in 0..TREE_DEPTH_MAX {
        if pid <= 1 || pid == root {
            return false;
        }
        match parent_of(pid) {
            Some(parent) if parent == root => return true,
            Some(parent) => pid = parent,
            None => return false,
        }
    }
    false
}

/// What `/proc/<pid>/stat` tells about a process's place: its parent, its process group
/// and its session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub parent: i32,
    pub group: i32,
    pub session: i32,
}

/// Reads the parent, group and session from the text of `/proc/<pid>/stat`, past a
/// program name that may hold spaces and brackets. Input: the text. Output: the fields,
/// or none when the text is not in that form.
pub fn stat(text: &str) -> Option<Stat> {
    let (_, rest) = text.rsplit_once(')')?;
    let mut fields = rest.split_ascii_whitespace().skip(1);
    let mut number = || fields.next()?.parse::<i32>().ok().filter(|id| *id >= 0);
    Some(Stat {
        parent: number()?,
        group: number()?,
        session: number()?,
    })
}

/// Reads one `Name:\tvalue` line of a `/proc` status or fdinfo text. Inputs: the text and
/// the name. Output: the value without the space around it, or none when no line has
/// that name.
pub fn field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key == name).then(|| value.trim())
    })
}

/// Splits a path into the folder part and the last name, as a creating call reads it:
/// slashes at the end do not count. Input: the path bytes. Output: the folder part (empty
/// for a bare name, `/` for a name in the root) and the name, or none when the path is
/// empty or only slashes.
pub fn split_last(path: &[u8]) -> Option<(&[u8], &[u8])> {
    let end = path.iter().rposition(|byte| *byte != b'/')? + 1;
    let named = &path[..end];
    match named.iter().rposition(|byte| *byte == b'/') {
        Some(0) => Some((&named[..1], &named[1..])),
        Some(slash) => Some((&named[..slash], &named[slash + 1..])),
        None => Some((&named[..0], named)),
    }
}

/// Rewrites a path that begins with `/proc/self` or `/proc/thread-self` to name the agent
/// that asked, since here those names mean the supervisor. Inputs: the path bytes, the
/// agent's process id and thread id. Output: the path to resolve; other paths unchanged.
pub fn as_agent(path: &[u8], tgid: i32, tid: i32) -> Vec<u8> {
    assert!(tgid > 0 && tid > 0);
    let named = |own: &[u8]| {
        let rest = path.strip_prefix(own)?;
        (rest.is_empty() || rest[0] == b'/').then_some(rest)
    };
    let (head, rest) = match (named(b"/proc/self"), named(b"/proc/thread-self")) {
        (Some(rest), _) => (format!("/proc/{tgid}"), rest),
        (None, Some(rest)) => (format!("/proc/{tgid}/task/{tid}"), rest),
        (None, None) => return path.to_vec(),
    };
    [head.as_bytes(), rest].concat()
}

/// Rewrites a resolved folder that lies under the supervisor's own `/proc` entry to the
/// agent's, for a path that reached `/proc/self` through a link. Inputs: the real path,
/// the supervisor's process and thread ids, the agent's. Output: the agent's path, or
/// none when the folder is not under the supervisor's entry.
pub fn from_own(real: &Path, own: (i32, i32), agent: (i32, i32)) -> Option<PathBuf> {
    assert!(own.0 > 0 && own.1 > 0 && agent.0 > 0 && agent.1 > 0);
    let thread = format!("/proc/{}/task/{}", own.0, own.1);
    if let Ok(rest) = real.strip_prefix(thread) {
        let head = format!("/proc/{}/task/{}", agent.0, agent.1);
        return Some(Path::new(&head).join(rest));
    }
    let rest = real.strip_prefix(format!("/proc/{}", own.0)).ok()?;
    Some(Path::new(&format!("/proc/{}", agent.0)).join(rest))
}

/// Reads the file mode mask from the text of `/proc/<pid>/status`. Input: the text.
/// Output: the mask, or none when the line is missing or not octal.
pub fn umask(status: &str) -> Option<u32> {
    let mask = u32::from_str_radix(field(status, "Umask")?, 8).ok()?;
    (mask <= 0o777).then_some(mask)
}

/// The mode a new entry gets when the supervisor makes it for an agent: the asked mode
/// without the bits the agent's mask clears. Inputs: the asked mode and the mask. Output:
/// the mode to make the entry with, under a mask of zero.
pub fn mode_under(mode: u32, umask: u32) -> u32 {
    mode & !umask & 0o7777
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(ip: [u8; 4], port: u16) -> Vec<u8> {
        let mut bytes = AF_INET.to_ne_bytes().to_vec();
        bytes.extend(port.to_be_bytes());
        bytes.extend(ip);
        bytes.extend([0u8; 8]);
        bytes
    }

    fn v6(ip: Ipv6Addr, port: u16) -> Vec<u8> {
        let mut bytes = AF_INET6.to_ne_bytes().to_vec();
        bytes.extend(port.to_be_bytes());
        bytes.extend([0u8; 4]);
        bytes.extend(ip.octets());
        bytes.extend([0u8; 4]);
        bytes
    }

    #[test]
    fn an_address_is_read_as_the_kernel_reads_it_and_a_short_or_unknown_one_is_other() {
        let local = v4([127, 0, 0, 1], 8080);
        assert_eq!(local.len(), 16);
        let read = Address::V4 {
            ip: [127, 0, 0, 1],
            port: 8080,
        };
        assert_eq!(address(&local), read);
        assert_eq!(address(&local[..8]), read, "the kernel judges the length");
        assert_eq!(address(&local[..7]), Address::Other);
        let six = v6("2001:db8::7".parse().unwrap(), 443);
        assert_eq!(six.len(), 28);
        let read = Address::V6 {
            ip: "2001:db8::7".parse::<Ipv6Addr>().unwrap().octets(),
            port: 443,
        };
        assert_eq!(address(&six), read);
        assert_eq!(address(&six[..24]), read);
        assert_eq!(address(&six[..23]), Address::Other);
        let mut unix = 1u16.to_ne_bytes().to_vec();
        unix.extend(b"/run/docker.sock\0");
        assert_eq!(address(&unix), Address::Other);
        let unspecified = [0u8; 16];
        assert_eq!(address(&unspecified), Address::Other);
        assert_eq!(address(&[]), Address::Other);
        assert_eq!(address(&[2]), Address::Other);
    }

    #[test]
    fn a_port_on_this_computer_is_reached_on_loopback_and_on_no_other_host() {
        let reach = Reach {
            any: vec![443, 53],
            loopback: vec![8080, 11434, 443],
        };
        let goes = |bytes: Vec<u8>| reach.allows(&address(&bytes));
        assert!(goes(v4([127, 0, 0, 1], 8080)));
        assert!(goes(v4([127, 8, 9, 10], 11434)), "all of 127.0.0.0/8");
        assert!(goes(v6(Ipv6Addr::LOCALHOST, 8080)));
        assert!(goes(v6("::ffff:127.0.0.1".parse().unwrap(), 8080)));
        for host in [[10, 0, 0, 5], [192, 168, 1, 1], [8, 8, 8, 8], [0, 0, 0, 0]] {
            assert!(!goes(v4(host, 8080)), "{host:?}");
            assert!(goes(v4(host, 443)), "{host:?}");
        }
        assert!(!goes(v4([128, 0, 0, 1], 8080)));
        for host in ["2001:db8::7", "::", "::ffff:10.0.0.5", "::127.0.0.1", "fe80::1"] {
            assert!(!goes(v6(host.parse().unwrap(), 11434)), "{host}");
            assert!(goes(v6(host.parse().unwrap(), 53)), "{host}");
        }
        assert!(!goes(v4([127, 0, 0, 1], 8081)), "a port no rule names");
        assert!(!goes(v4([127, 0, 0, 1], 0)));
        assert!(!reach.allows(&Address::Other));
        let none = Reach::default();
        assert!(!none.allows(&address(&v4([127, 0, 0, 1], 8080))));
    }

    fn own() -> GitOwn {
        GitOwn {
            trees: vec!["/work/project".into(), "/work/project/worktrees/a1".into()],
            kids: vec!["/work/clone/teammates".into()],
            exact: vec!["/work/setup/one/.git".into()],
        }
    }

    #[test]
    fn no_git_entry_is_made_outside_the_agents_own_git_folders() {
        let own = own();
        let made = |path: &str| own.allows(Path::new(path));
        for free in [
            "/work/project/src/main.rs",
            "/work/project/.gitignore",
            "/work/project/.github/workflows/ci.yml",
            "/work/project/a.git",
            "/work/project/.git",
            "/work/project/.git/index.lock",
            "/work/project/.git/modules/lib/config",
            "/work/project/worktrees/a1/.git",
            "/work/clone/teammates/bo/.git",
            "/work/clone/teammates/bo/.git/HEAD",
            "/work/setup/one/.git",
            "/work/setup/one/.git/objects/ab",
            "/tmp/build/out.o",
        ] {
            assert!(made(free), "{free}");
        }
        for refused in [
            "/work/project/.git/modules/lib/.git",
            "/work/project/.git/modules/lib/.git/config",
            "/work/project/.git/.git",
            "/work/clone/teammates/bo/.git/x/.GIT/config",
            "/work/setup/one/.git/.git",
            "/work/project/sub/.git",
            "/work/project/sub/.git/config",
            "/work/project/sub/.git/hooks/pre-commit",
            "/work/project/.GIT",
            "/work/project/.Git/config",
            "/work/project/sub/.gIt",
            "/work/project/worktrees/a1/deep/.git",
            "/work/project/worktrees/a2/.git",
            "/work/clone/teammates/.git",
            "/work/clone/teammates/bo/x/.git",
            "/work/clone/.git",
            "/work/setup/two/.git",
            "/work/setup/one/.git2/../.git",
            "/tmp/.git",
            "/tmp/build/.git/config",
            "/.git",
        ] {
            assert!(!made(refused), "{refused}");
        }
        for unformed in ["work/project/src", "", "/work/../work/project/a", "sub/.git"] {
            assert!(!made(unformed), "{unformed}");
        }
        assert!(made("/"), "the root holds no .git name");
        let bare = GitOwn::default();
        assert!(bare.allows(Path::new("/work/project/src/main.rs")));
        assert!(!bare.allows(Path::new("/work/project/.git")));
    }

    #[test]
    fn a_name_is_git_in_any_letter_case_and_nothing_longer_or_shorter_is() {
        for name in [".git", ".GIT", ".Git", ".gIT"] {
            assert!(is_git(OsStr::new(name)), "{name}");
        }
        for name in ["git", ".gi", ".gitignore", ".git ", " .git", ".git.", "..git", ""] {
            assert!(!is_git(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn a_signal_register_names_one_process_a_group_the_own_group_or_everyone() {
        assert_eq!(signal_target(4242), Target::One(4242));
        assert_eq!(signal_target(0), Target::OwnGroup);
        assert_eq!(signal_target(-1), Target::Everyone);
        assert_eq!(signal_target(-4242), Target::Group(4242));
        assert_eq!(signal_target(i64::from(i32::MIN)), Target::Everyone);
        assert_eq!(signal_target(0x7_0000_0001), Target::One(1), "the low 32 bits");
        assert_eq!(signal_target(0xffff_ffff), Target::Everyone, "minus one as 32 bits");
        assert_eq!(owner_target(0), None);
        assert_eq!(owner_target(4242), Some(Target::One(4242)));
        assert_eq!(owner_target(-4242), Some(Target::Group(4242)));
        assert_eq!(owner_target(-1), Some(Target::Group(1)));
        assert_eq!(owner_target(i64::from(i32::MIN)), Some(Target::Everyone));
    }

    #[test]
    fn a_signal_goes_only_to_a_program_the_agent_started() {
        let parents = [(300, 200), (200, 100), (100, 50), (60, 1), (70, 60), (50, 1)];
        let parent_of = |pid: i32| {
            let found = parents.iter().find(|(child, _)| *child == pid);
            found.map(|(_, parent)| *parent)
        };
        let inside = |pid| in_tree(pid, 100, parent_of);
        assert!(inside(200), "a child of the supervisor");
        assert!(inside(300), "a grandchild");
        assert!(!inside(100), "the supervisor itself");
        assert!(!inside(50), "the supervisor's own parent");
        assert!(!inside(60), "another program of the owner");
        assert!(!inside(70), "its child");
        assert!(!inside(1), "init");
        assert!(!inside(0));
        assert!(!inside(-5));
        assert!(!inside(999), "a pid no one has");
        let chain = |pid: i32| Some(pid - 1);
        let top = 2 + TREE_DEPTH_MAX as i32;
        assert!(in_tree(top, 2, chain), "the deepest chain that is walked");
        assert!(!in_tree(top + 1, 2, chain), "one deeper is refused, not walked");
        let looped = |pid: i32| Some(if pid == 7 { 8 } else { 7 });
        assert!(!in_tree(7, 100, looped), "a loop ends");
    }

    #[test]
    fn the_process_fields_are_read_past_a_name_with_spaces_and_brackets() {
        let plain = "4242 (node) S 4000 4242 3999 34816 4242 4194304 1 0";
        let read = Stat {
            parent: 4000,
            group: 4242,
            session: 3999,
        };
        assert_eq!(stat(plain), Some(read));
        let odd = "4242 (a) R 1 2 (b c)) S 4000 4242 3999 0";
        assert_eq!(stat(odd), Some(read), "the last bracket ends the name");
        for bad in ["", "4242 (node", "4242 (node) S", "4242 (node) S x 1 2", "1 (a) S -1 2 3"] {
            assert_eq!(stat(bad), None, "{bad}");
        }
        let status = "Name:\tnode\nUmask:\t0022\nState:\tS (sleeping)\nTgid:\t4242\nPid:\t4250\n";
        assert_eq!(field(status, "Tgid"), Some("4242"));
        assert_eq!(field(status, "Pid"), Some("4250"));
        assert_eq!(field(status, "Umask"), Some("0022"));
        assert_eq!(field(status, "State"), Some("S (sleeping)"));
        assert_eq!(field(status, "PPid"), None);
        assert_eq!(field(status, "gid"), None, "the whole name");
        assert_eq!(field("pos:\t0\nflags:\t02000002\nPid:\t77\n", "Pid"), Some("77"));
    }

    #[test]
    fn a_folder_that_holds_an_own_git_folder_is_not_moved_away() {
        let own = own();
        let moved = |path: &str| own.moves(Path::new(path));
        for free in [
            "/work/project/src",
            "/work/project/src/main.rs",
            "/work/project/.git/index.lock",
            "/work/project/worktrees/a1/src",
            "/work/clone/teammates/bo/src",
            "/work/clone/src",
            "/work/setup/two",
            "/tmp/build",
        ] {
            assert!(moved(free), "{free}");
        }
        for held in [
            "/work/project",
            "/work",
            "/",
            "/work/project/worktrees",
            "/work/project/worktrees/a1",
            "/work/clone",
            "/work/clone/teammates",
            "/work/clone/teammates/bo",
            "/work/clone/teammates/any",
            "/work/setup",
            "/work/setup/one",
        ] {
            assert!(!moved(held), "{held}");
        }
        assert!(!moved("work/project/src"), "not a real path");
        assert!(GitOwn::default().moves(Path::new("/work/project")));
    }

    #[test]
    fn a_path_splits_into_its_folder_and_last_name_whatever_slashes_end_it() {
        let split = |path: &'static str| {
            let parts = split_last(path.as_bytes())?;
            Some((std::str::from_utf8(parts.0).unwrap(), std::str::from_utf8(parts.1).unwrap()))
        };
        assert_eq!(split("a/b"), Some(("a", "b")));
        assert_eq!(split("b"), Some(("", "b")));
        assert_eq!(split("/b"), Some(("/", "b")));
        assert_eq!(split("/a/b/.git"), Some(("/a/b", ".git")));
        assert_eq!(split("a/b///"), Some(("a", "b")));
        assert_eq!(split("a//b"), Some(("a/", "b")));
        assert_eq!(split("a/.."), Some(("a", "..")));
        assert_eq!(split("./x"), Some((".", "x")));
        for none in ["", "/", "///"] {
            assert_eq!(split(none), None, "{none}");
        }
    }

    #[test]
    fn the_supervisors_own_proc_names_are_read_as_the_agents() {
        let agent = |path: &str| String::from_utf8(as_agent(path.as_bytes(), 4242, 4250)).unwrap();
        assert_eq!(agent("/proc/self/fd/2"), "/proc/4242/fd/2");
        assert_eq!(agent("/proc/self"), "/proc/4242");
        assert_eq!(agent("/proc/thread-self/comm"), "/proc/4242/task/4250/comm");
        assert_eq!(agent("/proc/thread-self"), "/proc/4242/task/4250");
        for kept in ["/proc/selfish/x", "/proc/1/fd/2", "proc/self/fd/2", "/work/proc/self", ""] {
            assert_eq!(agent(kept), kept);
        }
        let moved = |real: &str| from_own(Path::new(real), (900, 907), (4242, 4250));
        assert_eq!(moved("/proc/900/fd"), Some("/proc/4242/fd".into()));
        assert_eq!(moved("/proc/900"), Some("/proc/4242".into()));
        assert_eq!(moved("/proc/900/task/907/fd"), Some("/proc/4242/task/4250/fd".into()));
        assert_eq!(moved("/proc/900/task/908"), Some("/proc/4242/task/908".into()));
        for kept in ["/proc/9000/fd", "/proc/4242/fd", "/work/proc/900", "/proc"] {
            assert_eq!(moved(kept), None, "{kept}");
        }
    }

    #[test]
    fn a_new_entry_gets_the_asked_mode_without_what_the_agents_mask_clears() {
        let status = "Name:\tnode\nUmask:\t0027\nState:\tS (sleeping)\n";
        assert_eq!(umask(status), Some(0o027));
        assert_eq!(umask("Name:\tnode\n"), None);
        assert_eq!(umask("Umask:\t0x12\n"), None);
        assert_eq!(umask("Umask:\t7777\n"), None, "more than a mask holds");
        assert_eq!(mode_under(0o666, 0o027), 0o640);
        assert_eq!(mode_under(0o777, 0o022), 0o755);
        assert_eq!(mode_under(0o2775, 0), 0o2775);
        assert_eq!(mode_under(0o100644, 0o077), 0o600, "only mode bits");
    }
}
