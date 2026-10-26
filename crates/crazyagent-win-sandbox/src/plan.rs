use crate::profile::{BoundaryProfile, Guard};

/// File rights for reading and running: `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`.
pub const READ_EXECUTE: u32 = 0x0012_00a9;
/// File rights for changing contents: read, write, run and delete, without changing the access
/// list or owner (`FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE`).
pub const MODIFY: u32 = 0x0013_01bf;
/// File rights a read-only path denies: writing data, attributes and extended attributes,
/// adding entries, deleting the path or its children, and changing its access list or owner.
pub const DENY_WRITE: u32 = 0x000d_0156;
/// Every file right (`FILE_ALL_ACCESS`), denied on a hidden path.
pub const DENY_ALL: u32 = 0x001f_01ff;
/// File rights a folder on the way to a read-only path denies on itself only: adding files or
/// folders, deleting children, deleting or renaming the folder, and changing its access list or
/// owner, so no agent can vacate, replace or newly create the read-only path inside it.
pub const KEEP_ENTRIES: u32 = 0x000d_0046;

/// One access entry the boundary puts on a path for the agent's container: read and run, or
/// modify, on a granted root; deny writes, or deny everything, on a protected path; keep the
/// entries of a folder that holds a read-only path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    ReadExecute,
    Modify,
    DenyWrite,
    DenyAll,
    KeepEntries,
}

impl Access {
    /// The file rights this entry grants or denies. Input: the access. Output: the mask.
    pub fn mask(self) -> u32 {
        match self {
            Self::ReadExecute => READ_EXECUTE,
            Self::Modify => MODIFY,
            Self::DenyWrite => DENY_WRITE,
            Self::DenyAll => DENY_ALL,
            Self::KeepEntries => KEEP_ENTRIES,
        }
    }

    /// Whether this entry denies rights instead of granting them.
    pub fn denies(self) -> bool {
        matches!(self, Self::DenyWrite | Self::DenyAll | Self::KeepEntries)
    }

    /// Whether files and folders below a folder inherit this entry; a kept folder's entry holds
    /// for the folder itself only, so work inside it goes on.
    pub fn inherits(self) -> bool {
        self != Self::KeepEntries
    }
}

/// One planned access entry: the path and the access it gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ace {
    pub path: String,
    pub access: Access,
}

/// Whether `child` is `parent` or sits inside it, comparing whole path parts without regard to
/// case as Windows does. Inputs: two plain drive paths. Output: true when inside.
pub fn within(child: &str, parent: &str) -> bool {
    let child: Vec<&str> = child.split('\\').collect();
    let parent: Vec<&str> = parent.split('\\').collect();
    parent.len() <= child.len()
        && parent
            .iter()
            .zip(&child)
            .all(|(a, b)| a.eq_ignore_ascii_case(b) || a.to_uppercase() == b.to_uppercase())
}

/// Turns a profile into the access entries the boundary applies, grants first: modify on each
/// writable root and read and run on each read root, skipping a root another grant already
/// covers; then deny writes on each read-only path inside a writable root and deny everything
/// on each hidden path inside any granted root; then keep the entries of every folder from that
/// writable root down to each read-only path, so the path is neither vacated by renaming a
/// folder above it nor created while missing. A protected path outside every grant needs no
/// entry, since the container reaches nothing it was not granted. Input: the profile. Output:
/// the entries, or a refusal when a writable root sits in a protected path, a read root sits
/// in a hidden one, or the grants folder sits in a writable root, where the agent could
/// rewrite the record of its own grants.
pub fn plan(profile: &BoundaryProfile) -> Result<Vec<Ace>, &'static str> {
    profile.validate().map_err(|error| error.reason)?;
    if profile
        .write_roots
        .iter()
        .any(|root| within(&profile.grants, root))
    {
        return Err("the grants folder sits in a writable root");
    }
    for item in &profile.guarded {
        if profile
            .write_roots
            .iter()
            .any(|root| within(root, &item.path))
        {
            return Err("a writable root sits in a protected path");
        }
        if item.guard == Guard::Hidden
            && profile
                .read_roots
                .iter()
                .any(|root| within(root, &item.path))
        {
            return Err("a read root sits in a hidden path");
        }
    }
    let writes = outermost(profile.write_roots.iter().map(String::as_str), &[]);
    let reads = outermost(profile.read_roots.iter().map(String::as_str), &writes);
    let mut aces: Vec<Ace> = writes
        .iter()
        .map(|root| Ace {
            path: (*root).to_owned(),
            access: Access::Modify,
        })
        .chain(reads.iter().map(|root| Ace {
            path: (*root).to_owned(),
            access: Access::ReadExecute,
        }))
        .collect();
    let grants = aces.len();
    for item in &profile.guarded {
        let (access, covered) = match item.guard {
            Guard::ReadOnly => (
                Access::DenyWrite,
                writes.iter().any(|root| within(&item.path, root)),
            ),
            Guard::Hidden => (
                Access::DenyAll,
                writes
                    .iter()
                    .chain(&reads)
                    .any(|root| within(&item.path, root)),
            ),
        };
        let ace = Ace {
            path: item.path.clone(),
            access,
        };
        if covered && !aces.contains(&ace) {
            aces.push(ace);
        }
    }
    let denies = aces.len();
    for item in profile
        .guarded
        .iter()
        .filter(|item| item.guard == Guard::ReadOnly)
    {
        let Some(root) = writes.iter().find(|root| within(&item.path, root)) else {
            continue;
        };
        let parts: Vec<&str> = item.path.split('\\').collect();
        for end in root.split('\\').count()..parts.len() {
            let ace = Ace {
                path: parts[..end].join("\\"),
                access: Access::KeepEntries,
            };
            if !aces.contains(&ace) {
                aces.push(ace);
            }
        }
    }
    assert!(aces[..grants].iter().all(|ace| !ace.access.denies()));
    assert!(aces[grants..].iter().all(|ace| ace.access.denies()));
    assert!(aces[denies..].iter().all(|ace| {
        ace.access == Access::KeepEntries && writes.iter().any(|root| within(&ace.path, root))
    }));
    let parts: usize = profile
        .guarded
        .iter()
        .map(|item| item.path.split('\\').count())
        .sum();
    assert!(
        aces.len()
            <= profile.write_roots.len() + profile.read_roots.len() + profile.guarded.len() + parts
    );
    Ok(aces)
}

fn outermost<'a>(roots: impl Iterator<Item = &'a str>, covered: &[&'a str]) -> Vec<&'a str> {
    let mut kept: Vec<&'a str> = Vec::new();
    for root in roots {
        if covered.iter().chain(&kept).any(|outer| within(root, outer)) {
            continue;
        }
        kept.retain(|inner| !within(inner, root));
        kept.push(root);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Capability, Guarded};

    const PROJECT: &str = r"C:\Users\mira\Projects\shop";
    const HOME: &str = r"C:\Users\mira\AppData\Local\CrazyAgent\agents\a1\home";
    const TMP: &str = r"C:\Users\mira\AppData\Local\CrazyAgent\agents\a1\tmp";

    fn profile(read: &[&str], write: &[&str], guarded: &[(&str, Guard)]) -> BoundaryProfile {
        BoundaryProfile {
            container: "crazyagent.test".into(),
            capabilities: vec![Capability::InternetClient],
            read_roots: read.iter().map(|path| (*path).to_owned()).collect(),
            write_roots: write.iter().map(|path| (*path).to_owned()).collect(),
            guarded: guarded
                .iter()
                .map(|(path, guard)| Guarded {
                    path: (*path).to_owned(),
                    guard: *guard,
                })
                .collect(),
            active_processes: 64,
            grants: r"C:\Users\mira\AppData\Local\CrazyAgent\engine\boundary-grants".into(),
        }
    }

    fn ace(path: &str, access: Access) -> Ace {
        Ace {
            path: path.into(),
            access,
        }
    }

    #[test]
    fn the_masks_are_the_windows_file_rights() {
        let generic_read = 0x0012_0089;
        let generic_write = 0x0012_0116;
        let generic_execute = 0x0012_00a0;
        let delete = 0x0001_0000;
        assert_eq!(READ_EXECUTE, generic_read | generic_execute);
        assert_eq!(
            MODIFY,
            generic_read | generic_write | generic_execute | delete
        );
        assert_eq!(
            DENY_WRITE,
            0x2 | 0x4 | 0x10 | 0x40 | 0x100 | delete | 0x0004_0000 | 0x0008_0000
        );
        assert_eq!(DENY_WRITE & 0x0010_0000, 0, "reads keep SYNCHRONIZE");
        assert_eq!(
            MODIFY & (0x40 | 0x0004_0000 | 0x0008_0000),
            0,
            "modify never changes access lists or deletes children"
        );
        assert_eq!(DENY_ALL, 0x001f_01ff);
        assert_eq!(
            KEEP_ENTRIES,
            0x2 | 0x4 | 0x40 | delete | 0x0004_0000 | 0x0008_0000
        );
        assert_eq!(
            KEEP_ENTRIES & 0x0012_00a9,
            0,
            "a kept folder stays readable"
        );
        assert!(Access::DenyAll.denies() && Access::DenyWrite.denies());
        assert!(Access::KeepEntries.denies() && !Access::KeepEntries.inherits());
        assert!(!Access::Modify.denies() && !Access::ReadExecute.denies());
        assert!(Access::Modify.inherits() && Access::DenyWrite.inherits());
    }

    #[test]
    fn an_agent_gets_modify_on_its_roots_and_read_only_project_paths() {
        let context = r"C:\Users\mira\Projects\shop\context";
        let hooks = r"C:\Users\mira\Projects\shop\.git\hooks";
        let ssh = r"C:\Users\mira\.ssh";
        let planned = plan(&profile(
            &[r"C:\Program Files\nodejs"],
            &[PROJECT, HOME, TMP],
            &[
                (context, Guard::ReadOnly),
                (hooks, Guard::ReadOnly),
                (ssh, Guard::Hidden),
            ],
        ))
        .unwrap();
        assert_eq!(
            planned,
            [
                ace(PROJECT, Access::Modify),
                ace(HOME, Access::Modify),
                ace(TMP, Access::Modify),
                ace(r"C:\Program Files\nodejs", Access::ReadExecute),
                ace(context, Access::DenyWrite),
                ace(hooks, Access::DenyWrite),
                ace(PROJECT, Access::KeepEntries),
                ace(r"C:\Users\mira\Projects\shop\.git", Access::KeepEntries),
            ]
        );
        assert!(
            !planned.iter().any(|item| item.path == ssh),
            "a secret outside every grant gets no entry"
        );
    }

    #[test]
    fn covered_roots_collapse_and_paths_compare_without_case() {
        let planned = plan(&profile(
            &[
                r"c:\users\mira\projects\SHOP\vendor",
                r"C:\Tools",
                r"C:\Tools\bin",
            ],
            &[r"C:\Users\mira\Projects\shop\sub", PROJECT, PROJECT],
            &[],
        ))
        .unwrap();
        assert_eq!(
            planned,
            [
                ace(PROJECT, Access::Modify),
                ace(r"C:\Tools", Access::ReadExecute)
            ]
        );
        assert!(within(r"C:\A\b\C", r"c:\a\B"));
        assert!(within(r"C:\Ä\x", r"C:\ä"));
        assert!(!within(r"C:\ab", r"C:\a"));
        assert!(!within(r"C:\a", r"C:\a\b"));
    }

    #[test]
    fn protected_paths_outside_a_writable_root_need_no_deny() {
        let planned = plan(&profile(
            &[r"C:\Tools"],
            &[PROJECT],
            &[
                (r"C:\Tools\config", Guard::ReadOnly),
                (r"C:\Tools\keys", Guard::Hidden),
            ],
        ))
        .unwrap();
        assert_eq!(
            planned,
            [
                ace(PROJECT, Access::Modify),
                ace(r"C:\Tools", Access::ReadExecute),
                ace(r"C:\Tools\keys", Access::DenyAll),
            ]
        );
    }

    #[test]
    fn a_root_inside_a_protected_path_is_refused() {
        let context = r"C:\Users\mira\Projects\shop\context";
        assert_eq!(
            plan(&profile(
                &[],
                &[PROJECT, context],
                &[(context, Guard::ReadOnly)]
            )),
            Err("a writable root sits in a protected path")
        );
        assert_eq!(
            plan(&profile(&[], &[context], &[(PROJECT, Guard::Hidden)])),
            Err("a writable root sits in a protected path")
        );
        assert_eq!(
            plan(&profile(
                &[r"C:\Users\mira\.ssh\keys"],
                &[PROJECT],
                &[(r"C:\Users\mira\.ssh", Guard::Hidden)]
            )),
            Err("a read root sits in a hidden path")
        );
        assert!(
            plan(&profile(
                &[r"C:\Tools\x"],
                &[PROJECT],
                &[(r"C:\Tools", Guard::ReadOnly)]
            ))
            .is_ok()
        );
        let mut invalid = profile(&[], &[PROJECT], &[]);
        invalid.write_roots[0] = r"Projects\shop".into();
        assert!(plan(&invalid).is_err());
    }

    #[test]
    fn a_grants_folder_an_agent_could_write_is_refused() {
        let mut inside = profile(&[], &[PROJECT, HOME, TMP], &[]);
        for grants in [
            HOME.to_owned(),
            format!(r"{HOME}\grants"),
            PROJECT.to_lowercase(),
        ] {
            inside.grants = grants;
            assert_eq!(
                plan(&inside),
                Err("the grants folder sits in a writable root"),
                "{}",
                inside.grants
            );
        }
        let read = r"C:\Users\mira\AppData\Local\CrazyAgent\engine";
        let mut readable = profile(&[read], &[PROJECT, HOME, TMP], &[]);
        readable.grants = format!(r"{read}\boundary-grants");
        assert!(plan(&readable).is_ok(), "an agent may read its own record");
    }

    #[test]
    fn folders_holding_a_read_only_path_keep_their_entries_so_the_path_cannot_be_vacated() {
        let git = r"C:\Users\mira\Projects\shop\.git";
        let state = r"C:\Users\mira\Projects\shop\.crazyagent";
        let protected = [
            r"C:\Users\mira\Projects\shop\context",
            r"C:\Users\mira\Projects\shop\.crazyagent\context",
            r"C:\Users\mira\Projects\shop\.git\hooks",
            r"C:\Users\mira\Projects\shop\.git\config",
            r"C:\Users\mira\Projects\shop\.git\config.lock",
        ];
        let guarded: Vec<(&str, Guard)> = protected
            .iter()
            .map(|path| (*path, Guard::ReadOnly))
            .chain([(r"C:\Users\mira\Projects\shop\.git\secret", Guard::Hidden)])
            .collect();
        let planned = plan(&profile(&[], &[PROJECT, HOME, TMP], &guarded)).unwrap();
        let kept: Vec<&str> = planned
            .iter()
            .filter(|item| item.access == Access::KeepEntries)
            .map(|item| item.path.as_str())
            .collect();
        assert_eq!(
            kept,
            [PROJECT, state, git],
            "no agent may rename, delete or add an entry in a folder on the way to a read-only path"
        );
        for path in protected {
            assert!(planned.contains(&ace(path, Access::DenyWrite)));
        }

        let confined = plan(&profile(
            &[],
            &[
                r"C:\Users\mira\Projects\shop\.crazyagent\worktrees\agent-1",
                r"C:\Users\mira\Projects\shop\.git\worktrees\agent-1",
                r"C:\Users\mira\Projects\shop\.git\objects",
                HOME,
            ],
            &guarded,
        ))
        .unwrap();
        assert!(
            confined.iter().all(|item| !item.access.denies()),
            "read-only paths outside every writable root need no entry"
        );
    }

    #[test]
    fn a_path_both_read_only_and_hidden_gets_both_denies_once() {
        let keys = r"C:\Users\mira\Projects\shop\keys";
        let planned = plan(&profile(
            &[],
            &[PROJECT],
            &[
                (keys, Guard::ReadOnly),
                (keys, Guard::Hidden),
                (keys, Guard::ReadOnly),
            ],
        ))
        .unwrap();
        assert_eq!(
            planned,
            [
                ace(PROJECT, Access::Modify),
                ace(keys, Access::DenyWrite),
                ace(keys, Access::DenyAll),
                ace(PROJECT, Access::KeepEntries)
            ]
        );
    }
}
