#![forbid(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::io;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::path::{Component, Path, PathBuf};

pub const AF_UNSPEC: u16 = 0;
pub const AF_UNIX: u16 = 1;
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

/// A connect's address in the form the supervisor connects with for the agent: an IP
/// address with its flow and scope, a Unix socket's path or abstract name, the address
/// that ends a connection, or a family the supervisor does not carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dest {
    Ip(SocketAddr),
    UnixPath(Vec<u8>),
    UnixName(Vec<u8>),
    Unspec,
    Other,
}

/// Reads a socket address whole, for the connect made on the agent's behalf. Input: the
/// copied bytes, at most [`ADDRESS_BYTES_MAX`]. Output: the destination; a Unix path
/// ends at its first zero byte, an abstract name keeps every byte after the leading zero.
pub fn dest(bytes: &[u8]) -> Dest {
    let Some(family) = bytes.first_chunk::<2>() else {
        return Dest::Other;
    };
    let word = |at: usize| bytes.get(at..at + 4)?.first_chunk::<4>().copied();
    match (u16::from_ne_bytes(*family), address(bytes)) {
        (_, Address::V4 { ip, port }) => Dest::Ip(SocketAddr::from((ip, port))),
        (_, Address::V6 { ip, port }) => {
            let flow = word(4).map_or(0, u32::from_be_bytes);
            let scope = word(24).map_or(0, u32::from_ne_bytes);
            Dest::Ip(SocketAddrV6::new(ip.into(), port, flow, scope).into())
        }
        (AF_UNSPEC, _) => Dest::Unspec,
        (AF_UNIX, _) => match &bytes[2..] {
            [0, name @ ..] => Dest::UnixName(name.to_vec()),
            path => {
                let end = path.iter().position(|byte| *byte == 0);
                Dest::UnixPath(path[..end.unwrap_or(path.len())].to_vec())
            }
        },
        _ => Dest::Other,
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

/// The code points a case-folding folder leaves out when it compares names (Unicode's
/// default ignorable ones), as first and last of each range.
pub const IGNORED: [(u32, u32); 17] = [
    (0x00ad, 0x00ad),
    (0x034f, 0x034f),
    (0x061c, 0x061c),
    (0x115f, 0x1160),
    (0x17b4, 0x17b5),
    (0x180b, 0x180f),
    (0x200b, 0x200f),
    (0x202a, 0x202e),
    (0x2060, 0x206f),
    (0x3164, 0x3164),
    (0xfe00, 0xfe0f),
    (0xfeff, 0xfeff),
    (0xffa0, 0xffa0),
    (0xfff0, 0xfff8),
    (0x1bca0, 0x1bca3),
    (0x1d173, 0x1d17a),
    (0xe0000, 0xe0fff),
];

/// Tells whether a name is `.git` to some filesystem: in any letter case, with dots and
/// spaces after it (vfat and exFAT drop them) and with code points a case-folding folder
/// ignores anywhere in it. Input: one path component. Output: true for such a name; the
/// agent's own git folders are spelled `.git` exactly ([`GitOwn`]).
pub fn is_git(name: &OsStr) -> bool {
    folded(name) == ".git"
}

/// Spells one name as the loosest filesystem compares it: without the code points a
/// case-folding folder ignores, without dots and spaces at its end, in lower case. Input:
/// one path component. Output: the spelling two names share when some folder reads them
/// as one.
fn folded(name: &OsStr) -> String {
    let text = String::from_utf8_lossy(name.as_encoded_bytes());
    let ignored = |point: &char| {
        let point = u32::from(*point);
        IGNORED
            .iter()
            .any(|(first, last)| (*first..=*last).contains(&point))
    };
    let kept: String = text.chars().filter(|point| !ignored(point)).collect();
    kept.trim_end_matches(['.', ' ']).to_ascii_lowercase()
}

/// Spells a real, absolute path name by name with [`folded`]. Input: the path. Output: the
/// names below the root, or none for a path that is relative or holds `.` or `..`.
fn folded_parts(path: &Path) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    for (at, part) in path.components().enumerate() {
        match part {
            Component::RootDir if at == 0 => {}
            Component::Normal(name) if at > 0 => parts.push(folded(name)),
            _ => return None,
        }
    }
    path.is_absolute().then_some(parts)
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
    /// of the entry, its folder resolved and no `.` or `..` in it. Output: true when every
    /// component named `.git` in any letter case is one of the agent's own, at most one,
    /// or there is none; false otherwise and for a path that is not in that form.
    pub fn allows(&self, real: &Path) -> bool {
        let (mut walked, mut owned) = (PathBuf::new(), 0u32);
        for part in real.components() {
            match part {
                Component::RootDir if walked.as_os_str().is_empty() => walked.push("/"),
                Component::Normal(name) if walked.is_absolute() => {
                    walked.push(name);
                    if is_git(name) && (owned > 0 || !self.owns(&walked)) {
                        return false;
                    }
                    owned += u32::from(is_git(name));
                }
                _ => return false,
            }
        }
        assert!(owned <= 1);
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

/// The entries an agent may not write, truncate, move, link or remove (DEC-185 S2): each
/// path in `entries`, and each path in `trees` with everything below it; and the `stores`,
/// trees where no symlink is made and no symlink or folder is moved or linked in (S2b).
/// Paths are real and absolute, as the engine names them from the launch's guarded list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Held {
    pub entries: Vec<PathBuf>,
    pub trees: Vec<PathBuf>,
    pub stores: Vec<PathBuf>,
}

impl Held {
    /// Tells whether nothing is held. Output: true with no entry, tree or store.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.trees.is_empty() && self.stores.is_empty()
    }

    /// Tells whether a path is held, each name compared as the loosest folder compares it
    /// ([`folded`]). Input: the real, absolute path of an entry, its folder resolved.
    /// Output: true for an entry, a path in a tree, and any path not in that form, and
    /// for every path while a held path is itself not in that form.
    pub fn holds(&self, real: &Path) -> bool {
        let Some(path) = folded_parts(real) else {
            return true;
        };
        let entry = |held: &PathBuf| folded_parts(held).is_none_or(|held| held == path);
        let tree = |held: &PathBuf| folded_parts(held).is_none_or(|held| path.starts_with(&held));
        self.entries.iter().any(entry) || self.trees.iter().any(tree)
    }

    /// Tells whether moving a path away would carry a held entry with it. Input: the real,
    /// absolute source path. Output: true when it is held or a folder above a held entry.
    pub fn pins(&self, real: &Path) -> bool {
        let Some(path) = folded_parts(real) else {
            return true;
        };
        let below = |held: &PathBuf| folded_parts(held).is_none_or(|held| held.starts_with(&path));
        self.holds(real) || self.entries.iter().chain(&self.trees).any(below)
    }

    /// Tells whether a path lies in a held store, where no symlink or folder may be put,
    /// each name compared as [`Held::holds`] compares it. Input: the real, absolute path of
    /// an entry. Output: true for a path in a store, and with any store held, for a path
    /// not in that form and for every path while a store is itself not in that form.
    pub fn stored(&self, real: &Path) -> bool {
        if self.stores.is_empty() {
            return false;
        }
        let Some(path) = folded_parts(real) else {
            return true;
        };
        let store = |held: &PathBuf| folded_parts(held).is_none_or(|held| path.starts_with(&held));
        self.stores.iter().any(store)
    }
}

/// Tells whether a held store takes an entry of a kind (DEC-185 S2b): every kind but a
/// symlink and a folder, either of which could carry git outside the store. Input: the
/// entry's mode as `stat` gives it. Output: true when the entry may be put there.
pub fn store_takes(mode: u32) -> bool {
    let (kind, symlink, folder) = (mode & 0o170_000, 0o120_000, 0o040_000);
    kind != symlink && kind != folder
}

/// One entry as the kernel knows it whatever its name: its device and inode numbers.
pub type Node = (u64, u64);

/// The held entries as the kernel knows them, taken once before the harness starts
/// (DEC-185 S2a): the node of each held entry and tree that exists, the node of each held
/// tree, the node of every folder above a held path, each held path's folder node with its
/// last name, for a name that does not exist yet, and the node of each held store that
/// exists (S2b).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HeldNodes {
    pub nodes: Vec<Node>,
    pub trees: Vec<Node>,
    pub above: Vec<Node>,
    pub names: Vec<(Node, OsString)>,
    pub stores: Vec<Node>,
}

impl HeldNodes {
    /// Takes the nodes of the held paths. Inputs: the held paths and a stat that does not
    /// follow a link, giving none for a path that does not exist. Output: the nodes, or an
    /// error when a path is not absolute, a stat fails otherwise or the folder of a held
    /// entry or tree does not exist; a store that does not exist has no node.
    pub fn taken(
        held: &Held,
        stat: impl Fn(&Path) -> io::Result<Option<Node>>,
    ) -> io::Result<Self> {
        let mut nodes = Self::default();
        let entries = held.entries.iter().map(|path| (path, false));
        for (path, tree) in entries.chain(held.trees.iter().map(|path| (path, true))) {
            let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
                return Err(io::ErrorKind::InvalidInput.into());
            };
            if !path.is_absolute() {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            if let Some(node) = stat(path)? {
                nodes.nodes.push(node);
                if tree {
                    nodes.trees.push(node);
                }
            }
            let there =
                |path: &Path| stat(path)?.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound));
            nodes.names.push((there(folder)?, name.to_os_string()));
            for above in path.ancestors().skip(1) {
                nodes.above.push(there(above)?);
            }
        }
        for store in &held.stores {
            if !store.is_absolute() || store.file_name().is_none() {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            nodes.stores.extend(stat(store)?);
        }
        nodes.above.sort_unstable();
        nodes.above.dedup();
        assert_eq!(nodes.names.len(), held.entries.len() + held.trees.len());
        assert!(nodes.stores.len() <= held.stores.len());
        assert!(nodes.trees.iter().all(|tree| nodes.nodes.contains(tree)));
        Ok(nodes)
    }

    /// Tells whether a target is held. Inputs: the target's node, none when it does not
    /// exist, and the nodes of its folder and each folder above it, nearest first. Output:
    /// true for a held node, or when a folder on that chain is a held tree.
    pub fn holds(&self, target: Option<Node>, chain: &[Node]) -> bool {
        target.is_some_and(|node| self.nodes.contains(&node))
            || chain.iter().any(|folder| self.trees.contains(folder))
    }

    /// Tells whether moving a target away would carry a held entry with it. Inputs: as
    /// [`HeldNodes::holds`]. Output: true when it is held or a folder above a held path.
    pub fn pins(&self, target: Option<Node>, chain: &[Node]) -> bool {
        self.holds(target, chain) || target.is_some_and(|node| self.above.contains(&node))
    }

    /// The held names that live in one folder, which a new entry there must not turn out to
    /// be. Input: the folder's node. Output: those last names.
    pub fn names_in(&self, folder: Node) -> Vec<&OsStr> {
        let here = self.names.iter().filter(|(parent, _)| *parent == folder);
        here.map(|(_, name)| name.as_os_str()).collect()
    }

    /// Tells whether a folder lies in a held store. Input: the nodes of the folder and of
    /// each folder above it, nearest first. Output: true when one of them is a store.
    pub fn stored(&self, chain: &[Node]) -> bool {
        chain.iter().any(|folder| self.stores.contains(folder))
    }
}

/// How a time-setting call lays out the access and modification times it names in the
/// caller's memory, each count a 64-bit word: whole seconds (`utime`), seconds and
/// microseconds (`utimes`, `futimesat`), or seconds and nanoseconds (`utimensat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stamps {
    Seconds,
    Micros,
    Nanos,
}

impl Stamps {
    /// How many bytes the two times take in the caller's memory. Output: the count.
    pub fn size(self) -> usize {
        match self {
            Self::Seconds => 16,
            Self::Micros | Self::Nanos => 32,
        }
    }

    /// Reads the two times as `utimensat` takes them, seconds and nanoseconds, leaving
    /// the nanosecond counts for the kernel to check. Input: [`Stamps::size`] bytes.
    /// Output: the access and modification times, or none for a microsecond count outside
    /// 0..1_000_000, which the kernel refuses with `EINVAL`.
    pub fn read(self, bytes: &[u8]) -> Option<[(i64, i64); 2]> {
        assert_eq!(bytes.len(), self.size());
        let words: Vec<i64> = bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|word| i64::from_ne_bytes(*word))
            .collect();
        let micros = |sec: i64, usec: i64| {
            if !(0..1_000_000).contains(&usec) {
                return None;
            }
            Some((sec, usec * 1000))
        };
        match self {
            Self::Seconds => Some([(words[0], 0), (words[1], 0)]),
            Self::Micros => Some([micros(words[0], words[1])?, micros(words[2], words[3])?]),
            Self::Nanos => Some([(words[0], words[1]), (words[2], words[3])]),
        }
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
        for host in [
            "2001:db8::7",
            "::",
            "::ffff:10.0.0.5",
            "::127.0.0.1",
            "fe80::1",
        ] {
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
            "/work/project/sub/.git.",
            "/work/project/sub/.g\u{200d}it/config",
            "/work/project/.git.",
            "/work/project/.git\u{200c}/hooks",
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
        for unformed in [
            "work/project/src",
            "",
            "/work/../work/project/a",
            "sub/.git",
        ] {
            assert!(!made(unformed), "{unformed}");
        }
        assert!(made("/"), "the root holds no .git name");
        let bare = GitOwn::default();
        assert!(bare.allows(Path::new("/work/project/src/main.rs")));
        assert!(!bare.allows(Path::new("/work/project/.git")));
    }

    #[test]
    fn a_name_is_git_in_any_letter_case_and_in_every_form_a_filesystem_reads_as_it() {
        for name in [".git", ".GIT", ".Git", ".gIT"] {
            assert!(is_git(OsStr::new(name)), "{name}");
        }
        let aliases = [
            ".git.",
            ".git ",
            ".git. .",
            ".g\u{200d}it",
            ".git\u{200c}",
            "\u{feff}.git",
            ".GI\u{ad}T.",
            ".gi\u{e0001}t",
            "\u{2060}.\u{fe0f}g\u{180e}i\u{3164}t\u{200b} ",
        ];
        for name in aliases {
            assert!(
                is_git(OsStr::new(name)),
                "a name some filesystem reads as .git: {name:?}"
            );
        }
        let others = [
            "git",
            ".gi",
            ".gitignore",
            " .git",
            "..git",
            "",
            ".",
            "x.git",
            "\u{65e5}.git",
            ".git2.",
            ".g\u{131}t",
            ".gi\u{fffd}t",
            ".git\u{e9}",
            ". git",
        ];
        for name in others {
            assert!(!is_git(OsStr::new(name)), "{name:?}");
        }
        let broken = std::os::unix::ffi::OsStrExt::from_bytes(b".gi\xfft");
        assert!(!is_git(broken), "bytes that are no text are no alias");
    }

    #[test]
    fn a_signal_register_names_one_process_a_group_the_own_group_or_everyone() {
        assert_eq!(signal_target(4242), Target::One(4242));
        assert_eq!(signal_target(0), Target::OwnGroup);
        assert_eq!(signal_target(-1), Target::Everyone);
        assert_eq!(signal_target(-4242), Target::Group(4242));
        assert_eq!(signal_target(i64::from(i32::MIN)), Target::Everyone);
        assert_eq!(
            signal_target(0x7_0000_0001),
            Target::One(1),
            "the low 32 bits"
        );
        assert_eq!(
            signal_target(0xffff_ffff),
            Target::Everyone,
            "minus one as 32 bits"
        );
        assert_eq!(owner_target(0), None);
        assert_eq!(owner_target(4242), Some(Target::One(4242)));
        assert_eq!(owner_target(-4242), Some(Target::Group(4242)));
        assert_eq!(owner_target(-1), Some(Target::Group(1)));
        assert_eq!(owner_target(i64::from(i32::MIN)), Some(Target::Everyone));
    }

    #[test]
    fn a_signal_goes_only_to_a_program_the_agent_started() {
        let parents = [
            (300, 200),
            (200, 100),
            (100, 50),
            (60, 1),
            (70, 60),
            (50, 1),
        ];
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
        assert!(
            !in_tree(top + 1, 2, chain),
            "one deeper is refused, not walked"
        );
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
        for bad in [
            "",
            "4242 (node",
            "4242 (node) S",
            "4242 (node) S x 1 2",
            "1 (a) S -1 2 3",
        ] {
            assert_eq!(stat(bad), None, "{bad}");
        }
        let status = "Name:\tnode\nUmask:\t0022\nState:\tS (sleeping)\nTgid:\t4242\nPid:\t4250\n";
        assert_eq!(field(status, "Tgid"), Some("4242"));
        assert_eq!(field(status, "Pid"), Some("4250"));
        assert_eq!(field(status, "Umask"), Some("0022"));
        assert_eq!(field(status, "State"), Some("S (sleeping)"));
        assert_eq!(field(status, "PPid"), None);
        assert_eq!(field(status, "gid"), None, "the whole name");
        assert_eq!(
            field("pos:\t0\nflags:\t02000002\nPid:\t77\n", "Pid"),
            Some("77")
        );
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

    fn held() -> Held {
        Held {
            entries: vec![
                PathBuf::from("/p/.git/worktrees/a/HEAD"),
                PathBuf::from("/p/.git/worktrees/a/commondir"),
                PathBuf::from("/p/worktrees/a/.git"),
            ],
            trees: vec![PathBuf::from("/p/.git/worktrees/a/modules")],
            stores: Vec::new(),
        }
    }

    #[test]
    fn a_held_entry_and_everything_in_a_held_tree_is_held_and_a_neighbour_is_not() {
        let held = held();
        for path in [
            "/p/.git/worktrees/a/HEAD",
            "/p/.git/worktrees/a/commondir",
            "/p/worktrees/a/.git",
            "/p/.git/worktrees/a/modules",
            "/p/.git/worktrees/a/modules/sub/config",
        ] {
            assert!(held.holds(Path::new(path)), "{path}");
        }
        for path in [
            "/p/.git/worktrees/a/HEAD.lock",
            "/p/.git/worktrees/a/HEADS",
            "/p/.git/worktrees/a/modulesx",
            "/p/.git/worktrees/a/index",
            "/p/worktrees/a/.gitignore",
            "/p/worktrees/a/src/.git",
        ] {
            assert!(!held.holds(Path::new(path)), "{path}");
        }
        for path in ["p/.git/worktrees/a/index", "/p/.git/worktrees/b/../a/index"] {
            assert!(held.holds(Path::new(path)), "{path} is not a real path");
        }
        assert!(!Held::default().holds(Path::new("/p/.git/worktrees/a/HEAD")));
    }

    #[test]
    fn a_held_entry_or_a_folder_above_one_is_never_moved_away() {
        let held = held();
        for path in [
            "/p/.git/worktrees/a",
            "/p/.git/worktrees",
            "/p/worktrees/a",
            "/p",
            "/",
            "/p/.git/worktrees/a/modules",
            "/p/.git/worktrees/a/modules/sub",
            "/p/.git/worktrees/a/HEAD",
        ] {
            assert!(held.pins(Path::new(path)), "{path}");
        }
        for path in [
            "/p/worktrees/a/src",
            "/p/.git/worktrees/a/index",
            "/p/.git/worktrees/a/index.lock",
            "/p/worktrees/b",
        ] {
            assert!(!held.pins(Path::new(path)), "{path}");
        }
        assert!(held.pins(Path::new("p/worktrees/a/src")), "not a real path");
    }

    fn nodes() -> HeldNodes {
        let held = held();
        let map = [
            ("/", (1, 2)),
            ("/p", (1, 10)),
            ("/p/.git", (1, 11)),
            ("/p/.git/worktrees", (1, 12)),
            ("/p/.git/worktrees/a", (1, 13)),
            ("/p/.git/worktrees/a/HEAD", (1, 14)),
            ("/p/.git/worktrees/a/modules", (1, 16)),
            ("/p/worktrees", (1, 20)),
            ("/p/worktrees/a", (1, 21)),
            ("/p/worktrees/a/.git", (1, 22)),
        ];
        let stat = |path: &Path| {
            let found = map.iter().find(|(at, _)| Path::new(at) == path);
            Ok(found.map(|(_, node)| *node))
        };
        HeldNodes::taken(&held, stat).unwrap()
    }

    #[test]
    fn the_held_nodes_are_taken_once_and_a_held_name_that_is_not_there_yet_is_kept() {
        let nodes = nodes();
        let mut found = nodes.nodes.clone();
        found.sort_unstable();
        assert_eq!(
            found,
            vec![(1, 14), (1, 16), (1, 22)],
            "commondir is not there yet"
        );
        assert_eq!(nodes.trees, vec![(1, 16)]);
        let mut above = nodes.above.clone();
        above.sort_unstable();
        above.dedup();
        assert_eq!(
            above,
            vec![(1, 2), (1, 10), (1, 11), (1, 12), (1, 13), (1, 20), (1, 21)]
        );
        assert_eq!(nodes.names.len(), 4);
        let mut admin = nodes.names_in((1, 13));
        admin.sort();
        assert_eq!(
            admin,
            vec![
                OsStr::new("HEAD"),
                OsStr::new("commondir"),
                OsStr::new("modules")
            ]
        );
        assert_eq!(nodes.names_in((1, 21)), vec![OsStr::new(".git")]);
        assert!(nodes.names_in((1, 99)).is_empty());
        let failing = |path: &Path| match path.to_str() {
            Some("/p/.git/worktrees/a/HEAD") => {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            }
            _ => Ok(Some((1, 1))),
        };
        assert!(HeldNodes::taken(&held(), failing).is_err());
        let no_folder = |path: &Path| Ok((path != Path::new("/p/worktrees/a")).then_some((1, 1)));
        assert!(
            HeldNodes::taken(&held(), no_folder).is_err(),
            "a held path's folder must be there"
        );
    }

    #[test]
    fn a_held_node_or_anything_in_a_held_tree_is_held_whatever_name_reaches_it() {
        let nodes = nodes();
        let admin = [(1, 13), (1, 12), (1, 11), (1, 10), (1, 2)];
        assert!(nodes.holds(Some((1, 14)), &admin), "HEAD by any name");
        assert!(
            nodes.holds(Some((1, 16)), &admin),
            "the modules tree itself"
        );
        let in_modules = [(1, 30), (1, 16), (1, 13), (1, 12), (1, 11), (1, 10), (1, 2)];
        assert!(nodes.holds(Some((1, 31)), &in_modules));
        assert!(nodes.holds(None, &in_modules), "a new name in the tree");
        assert!(!nodes.holds(Some((1, 15)), &admin), "the index");
        assert!(!nodes.holds(None, &admin), "a new lock file");
        let worktree = [(1, 21), (1, 20), (1, 10), (1, 2)];
        assert!(nodes.holds(Some((1, 22)), &worktree));
        assert!(!nodes.holds(Some((1, 40)), &worktree));
        assert!(!HeldNodes::default().holds(Some((1, 14)), &admin));
    }

    #[test]
    fn a_folder_above_a_held_entry_is_never_moved_and_a_sibling_folder_is() {
        let nodes = nodes();
        let root = [(1, 2)];
        assert!(nodes.pins(Some((1, 10)), &root), "the project");
        assert!(
            nodes.pins(Some((1, 13)), &[(1, 12), (1, 11), (1, 10), (1, 2)]),
            "the admin folder"
        );
        assert!(
            nodes.pins(Some((1, 21)), &[(1, 20), (1, 10), (1, 2)]),
            "the worktree"
        );
        assert!(nodes.pins(Some((1, 14)), &[(1, 13), (1, 12), (1, 11), (1, 10), (1, 2)]));
        let src = [(1, 21), (1, 20), (1, 10), (1, 2)];
        assert!(!nodes.pins(Some((1, 41)), &src), "a folder in the worktree");
        assert!(!nodes.pins(None, &src));
    }

    #[test]
    fn the_times_a_time_call_names_are_read_as_seconds_and_nanoseconds() {
        let bytes = |words: &[i64]| -> Vec<u8> {
            words.iter().flat_map(|word| word.to_ne_bytes()).collect()
        };
        assert_eq!(Stamps::Seconds.size(), 16);
        assert_eq!(Stamps::Micros.size(), 32);
        assert_eq!(Stamps::Nanos.size(), 32);
        let seconds = Stamps::Seconds.read(&bytes(&[7, 9]));
        assert_eq!(seconds, Some([(7, 0), (9, 0)]));
        let micros = Stamps::Micros.read(&bytes(&[7, 5, 9, 999_999]));
        assert_eq!(micros, Some([(7, 5_000), (9, 999_999_000)]));
        for bad in [[7, 1_000_000, 9, 0], [7, 0, 9, -1], [7, i64::MAX, 9, 0]] {
            assert_eq!(Stamps::Micros.read(&bytes(&bad)), None, "{bad:?}");
        }
        let omit = (1 << 30) - 2;
        let nanos = Stamps::Nanos.read(&bytes(&[7, 5, 9, omit]));
        assert_eq!(nanos, Some([(7, 5), (9, omit)]));
        let unchecked = Stamps::Nanos.read(&bytes(&[-1, 2_000_000_000, 0, 0]));
        assert_eq!(
            unchecked,
            Some([(-1, 2_000_000_000), (0, 0)]),
            "the kernel checks it"
        );
    }

    #[test]
    fn a_path_in_a_held_store_is_stored_by_name_and_a_neighbour_is_not() {
        let held = Held {
            stores: vec![PathBuf::from("/c/.git/objects")],
            ..Held::default()
        };
        for path in [
            "/c/.git/objects/ab/cd",
            "/c/.git/OBJECTS/pack/x",
            "/c/.git/objects",
        ] {
            assert!(held.stored(Path::new(path)), "{path}");
        }
        for path in [
            "/c/.git/refs/heads/x",
            "/c/objects/ab",
            "/c/.git/objects2/x",
        ] {
            assert!(!held.stored(Path::new(path)), "{path}");
        }
        assert!(
            held.stored(Path::new("c/.git/objects/ab")),
            "not a real path"
        );
        assert!(
            !Held::default().stored(Path::new("c/.git/objects/ab")),
            "no store held"
        );
        assert!(
            !held.holds(Path::new("/c/.git/objects/ab/cd")),
            "a store holds no entry"
        );
        assert!(!held.is_empty() && Held::default().is_empty());
    }

    #[test]
    fn a_store_takes_every_kind_but_a_symlink_and_a_folder() {
        for taken in [
            0o100_644, 0o100_755, 0o010_600, 0o140_755, 0o020_600, 0o060_600,
        ] {
            assert!(store_takes(taken), "{taken:o}");
        }
        for refused in [0o120_777, 0o040_755, 0o041_777] {
            assert!(!store_takes(refused), "{refused:o}");
        }
    }

    #[test]
    fn a_folder_whose_chain_runs_through_a_held_store_is_stored() {
        let held = Held {
            stores: vec![PathBuf::from("/c/.git/objects"), PathBuf::from("/c/gone")],
            ..Held::default()
        };
        let map = [
            ("/", (1, 2)),
            ("/c", (1, 40)),
            ("/c/.git", (1, 41)),
            ("/c/.git/objects", (1, 50)),
        ];
        let stat = |path: &Path| {
            let found = map.iter().find(|(at, _)| Path::new(at) == path);
            Ok(found.map(|(_, node)| *node))
        };
        let nodes = HeldNodes::taken(&held, stat).unwrap();
        assert_eq!(nodes.stores, vec![(1, 50)], "a store not there has no node");
        assert!(
            nodes.stored(&[(1, 51), (1, 50), (1, 41), (1, 40), (1, 2)]),
            "a fan-out"
        );
        assert!(
            nodes.stored(&[(1, 50), (1, 41), (1, 40), (1, 2)]),
            "the store itself"
        );
        assert!(!nodes.stored(&[(1, 41), (1, 40), (1, 2)]), "the git folder");
        assert!(
            !nodes.holds(Some((1, 50)), &[(1, 41)]),
            "a store is not held"
        );
        let relative = Held {
            stores: vec![PathBuf::from("c/.git/objects")],
            ..Held::default()
        };
        assert!(HeldNodes::taken(&relative, stat).is_err());
    }

    #[test]
    fn a_path_splits_into_its_folder_and_last_name_whatever_slashes_end_it() {
        let split = |path: &'static str| {
            let parts = split_last(path.as_bytes())?;
            Some((
                std::str::from_utf8(parts.0).unwrap(),
                std::str::from_utf8(parts.1).unwrap(),
            ))
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
        for kept in [
            "/proc/selfish/x",
            "/proc/1/fd/2",
            "proc/self/fd/2",
            "/work/proc/self",
            "",
        ] {
            assert_eq!(agent(kept), kept);
        }
        let moved = |real: &str| from_own(Path::new(real), (900, 907), (4242, 4250));
        assert_eq!(moved("/proc/900/fd"), Some("/proc/4242/fd".into()));
        assert_eq!(moved("/proc/900"), Some("/proc/4242".into()));
        assert_eq!(
            moved("/proc/900/task/907/fd"),
            Some("/proc/4242/task/4250/fd".into())
        );
        assert_eq!(
            moved("/proc/900/task/908"),
            Some("/proc/4242/task/908".into())
        );
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

    #[test]
    fn a_connect_address_is_carried_whole_to_the_connect_made_for_the_agent() {
        let local = dest(&v4([127, 0, 0, 1], 8080));
        assert_eq!(local, Dest::Ip("127.0.0.1:8080".parse().unwrap()));
        let mut six = v6("fe80::1".parse().unwrap(), 443);
        six[4..8].copy_from_slice(&7u32.to_be_bytes());
        six[24..28].copy_from_slice(&3u32.to_ne_bytes());
        let Dest::Ip(SocketAddr::V6(read)) = dest(&six) else {
            panic!("not an IPv6 destination");
        };
        assert_eq!((read.port(), read.flowinfo(), read.scope_id()), (443, 7, 3));
        assert_eq!(read.ip(), &"fe80::1".parse::<Ipv6Addr>().unwrap());
        let Dest::Ip(SocketAddr::V6(short)) = dest(&six[..24]) else {
            panic!("not an IPv6 destination");
        };
        assert_eq!(short.scope_id(), 0, "no scope in a short address");
        let unix = |path: &[u8]| [&AF_UNIX.to_ne_bytes()[..], path].concat();
        let path = |path: &[u8]| Dest::UnixPath(path.to_vec());
        assert_eq!(
            dest(&unix(b"/run/user/1000/bus\0")),
            path(b"/run/user/1000/bus")
        );
        assert_eq!(dest(&unix(b"sock\0junk")), path(b"sock"));
        assert_eq!(
            dest(&unix(b"sock")),
            path(b"sock"),
            "no zero byte at the end"
        );
        assert_eq!(dest(&unix(b"")), path(b""));
        assert_eq!(
            dest(&unix(b"\0name\0x")),
            Dest::UnixName(b"name\0x".to_vec())
        );
        assert_eq!(dest(&[0u8; 16]), Dest::Unspec);
        assert_eq!(dest(&AF_UNSPEC.to_ne_bytes()), Dest::Unspec);
        let netlink = [&16u16.to_ne_bytes()[..], &[0u8; 10][..]].concat();
        assert_eq!(dest(&netlink), Dest::Other);
        assert_eq!(
            dest(&v4([127, 0, 0, 1], 8080)[..7]),
            Dest::Other,
            "a cut address"
        );
        assert_eq!(dest(&[]), Dest::Other);
        assert_eq!(dest(&[1]), Dest::Other);
    }
}
