use crate::profile::{BoundaryProfile, Guard};

/// File rights for reading and running: `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`.
pub const READ_EXECUTE: u32 = 0x0012_00a9;
/// File rights for changing contents: read, write, run and delete, without changing the access
/// list or owner (`FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE`).
pub const MODIFY: u32 = 0x0013_01bf;
/// The right to delete or rename a file or folder.
pub const DELETE: u32 = 0x0001_0000;
/// The rights to add a file and to add a folder inside a folder.
pub const ADD_ENTRIES: u32 = 0x2 | 0x4;
/// The rights an anchored folder gives on itself: modify without delete, so no agent can
/// delete or rename it to vacate a guarded path inside, while new entries such as git's
/// `.git\index.lock` can still be made. Nothing grants delete-child, so no entry inside is
/// deleted through the folder either.
pub const ANCHOR: u32 = MODIFY & !DELETE;
/// The rights the deepest present folder above a missing guarded path gives on itself: the
/// anchor's rights without adding entries, since Windows cannot refuse a single name, so no
/// agent can create the missing path or a folder on the way to it.
pub const KEEP_ENTRIES: u32 = ANCHOR & !ADD_ENTRIES;

/// The role the boundary gives the agent's container on one path. Granted roots: read and
/// run, modify, or modify with the root itself anchored or kept when it holds a guarded path.
/// Guarded paths: read only, or hidden. Folders between a writable root and a guarded path:
/// anchored, or kept above a missing one. No role ever denies: what a container is not
/// granted, it does not get.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    ReadExecute,
    Modify,
    RootAnchor,
    RootKeep,
    ReadOnly,
    Hidden,
    Anchor,
    KeepEntries,
}

/// The rights one role gives a container: on the path itself, and on the files and folders
/// below a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rights {
    pub this: u32,
    pub below: u32,
}

impl Access {
    /// The rights this role gives on the path and below it. Input: the role. Output: the
    /// rights.
    pub fn rights(self) -> Rights {
        let (this, below) = match self {
            Self::ReadExecute | Self::ReadOnly => (READ_EXECUTE, READ_EXECUTE),
            Self::Modify => (MODIFY, MODIFY),
            Self::Hidden => (0, 0),
            Self::Anchor | Self::RootAnchor => (ANCHOR, MODIFY),
            Self::KeepEntries | Self::RootKeep => (KEEP_ENTRIES, MODIFY),
        };
        assert_eq!(this & !MODIFY, 0);
        assert_eq!(below & !MODIFY, 0);
        Rights { this, below }
    }

    /// Whether the path's inheritance is cut, so it holds only explicit entries: guarded
    /// paths and the folders anchored or kept inside a writable root. A granted root keeps its
    /// inheritance; one holding guards is refused when what it inherits gives the container
    /// more on it than its role ([`crate::dacl::converge`]).
    pub fn cuts(self) -> bool {
        matches!(
            self,
            Self::ReadOnly | Self::Hidden | Self::Anchor | Self::KeepEntries
        )
    }

    /// Whether every other container's entries on the path are narrowed to this role's rights
    /// as well, so a teammate on the same project gets no more on a guarded or anchored path
    /// than this container does. A plain grant leaves other containers alone.
    pub fn narrows_others(self) -> bool {
        !matches!(self, Self::ReadExecute | Self::Modify)
    }
}

/// One planned role: the path and the role it gets.
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

/// Whether two paths name the same place, compared without regard to case.
pub fn same(a: &str, b: &str) -> bool {
    within(a, b) && within(b, a)
}

fn depth(path: &str) -> usize {
    path.split('\\').count()
}

/// Turns a profile into the roles the boundary applies, one per path, in the order it applies
/// them (DEC-193 D1): guarded paths deepest first, read only inside a writable root and hidden
/// inside any granted root, hidden winning when a path is both; then the folders between a
/// writable root and each guarded path in it, deepest first, anchored so the path is not
/// vacated, and the deepest present folder above a missing path kept, so it is not created;
/// then the roots, a writable root holding a guarded path anchored or kept itself. A guarded
/// path outside every grant needs no role, since the container reaches nothing it was not
/// granted. Inputs: the profile and whether a path is present as a real file or folder, not a
/// link or junction; a granted root counts as present. Output: the roles, or a refusal when a
/// writable root sits in a protected path, a read root sits in a hidden one, or the grants
/// folder sits in a writable root, where the agent could rewrite the record of its own grants.
pub fn plan(
    profile: &BoundaryProfile,
    present: impl Fn(&str) -> bool,
) -> Result<Vec<Ace>, &'static str> {
    refuse(profile)?;
    let writes = outermost(profile.write_roots.iter().map(String::as_str), &[]);
    let reads = outermost(profile.read_roots.iter().map(String::as_str), &writes);
    let mut guards = guards(profile, &writes, &reads);
    guards.sort_by_key(|ace| std::cmp::Reverse(depth(&ace.path)));
    let mut anchors = anchors(profile, &writes, &guards, present);
    anchors.sort_by_key(|ace| std::cmp::Reverse(depth(&ace.path)));
    let roots = writes
        .iter()
        .map(|root| {
            let anchored = anchors.iter().find(|ace| same(&ace.path, root));
            Ace {
                path: (*root).to_owned(),
                access: anchored.map_or(Access::Modify, |ace| ace.access),
            }
        })
        .chain(reads.iter().map(|root| Ace {
            path: (*root).to_owned(),
            access: Access::ReadExecute,
        }))
        .collect::<Vec<_>>();
    anchors.retain(|ace| ace.access.cuts());
    let planned: Vec<Ace> = guards.into_iter().chain(anchors).chain(roots).collect();
    assert!(planned.iter().enumerate().all(|(index, ace)| {
        planned[..index]
            .iter()
            .all(|seen| !same(&seen.path, &ace.path))
    }));
    assert!(planned.len() <= writes.len() + reads.len() + guarded_parts(profile));
    Ok(planned)
}

fn guarded_parts(profile: &BoundaryProfile) -> usize {
    profile
        .guarded
        .iter()
        .map(|item| depth(&item.path))
        .sum::<usize>()
}

/// The profile refusals the plan checks before building anything.
fn refuse(profile: &BoundaryProfile) -> Result<(), &'static str> {
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
    Ok(())
}

/// One role per guarded path a grant reaches: read only inside a writable root, hidden inside
/// any granted root, hidden winning for a path listed both ways.
fn guards(profile: &BoundaryProfile, writes: &[&str], reads: &[&str]) -> Vec<Ace> {
    let mut guards: Vec<Ace> = Vec::new();
    for item in &profile.guarded {
        let (access, covered) = match item.guard {
            Guard::ReadOnly => (
                Access::ReadOnly,
                writes.iter().any(|root| within(&item.path, root)),
            ),
            Guard::Hidden => (
                Access::Hidden,
                writes
                    .iter()
                    .chain(reads)
                    .any(|root| within(&item.path, root)),
            ),
        };
        if !covered {
            continue;
        }
        match guards.iter_mut().find(|ace| same(&ace.path, &item.path)) {
            Some(ace) if access == Access::Hidden => ace.access = Access::Hidden,
            Some(_) => {}
            None => guards.push(Ace {
                path: item.path.clone(),
                access,
            }),
        }
    }
    assert!(guards.iter().all(|ace| ace.access.cuts()));
    assert!(guards.len() <= profile.guarded.len());
    guards
}

/// The folders from a writable root down to each guarded path in it: anchored while present,
/// the deepest present one kept when the path or a folder on the way is missing, the root
/// itself as a root role. A walk stops at a guarded folder, whose own role already refuses
/// deleting and adding. Keeping wins over anchoring for a folder reached twice.
fn anchors(
    profile: &BoundaryProfile,
    writes: &[&str],
    guards: &[Ace],
    present: impl Fn(&str) -> bool,
) -> Vec<Ace> {
    let mut anchors: Vec<Ace> = Vec::new();
    for item in guards {
        let Some(root) = writes.iter().find(|root| within(&item.path, root)) else {
            continue;
        };
        let parts: Vec<&str> = item.path.split('\\').collect();
        let from = depth(root);
        let mut folders = Vec::new();
        for end in from..parts.len() {
            let folder = parts[..end].join("\\");
            if end > from && (!present(&folder) || guards.iter().any(|g| same(&g.path, &folder))) {
                break;
            }
            folders.push(folder);
        }
        let complete = folders.len() == parts.len() - from;
        let missing = !complete || !present(&item.path);
        let blocked = !complete
            && guards
                .iter()
                .any(|g| same(&g.path, &parts[..from + folders.len()].join("\\")));
        let deepest = folders.len() - 1;
        for (index, path) in folders.into_iter().enumerate() {
            let keep = missing && !blocked && index == deepest;
            let access = match (index == 0, keep) {
                (true, true) => Access::RootKeep,
                (true, false) => Access::RootAnchor,
                (false, true) => Access::KeepEntries,
                (false, false) => Access::Anchor,
            };
            match anchors.iter_mut().find(|ace| same(&ace.path, &path)) {
                Some(ace) if keep => ace.access = access,
                Some(_) => {}
                None => anchors.push(Ace { path, access }),
            }
        }
    }
    assert!(anchors.len() <= guarded_parts(profile));
    assert!(anchors.iter().all(|ace| ace.access.narrows_others()));
    anchors
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
    const GIT: &str = r"C:\Users\mira\Projects\shop\.git";
    const STATE: &str = r"C:\Users\mira\Projects\shop\.crazyagent";
    const CONTEXT: &str = r"C:\Users\mira\Projects\shop\context";
    const HOOKS: &str = r"C:\Users\mira\Projects\shop\.git\hooks";
    const CONFIG: &str = r"C:\Users\mira\Projects\shop\.git\config";

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

    fn plan(profile: &BoundaryProfile) -> Result<Vec<Ace>, &'static str> {
        super::plan(profile, |_| true)
    }

    fn roles(planned: &[Ace], access: Access) -> Vec<&str> {
        planned
            .iter()
            .filter(|item| item.access == access)
            .map(|item| item.path.as_str())
            .collect()
    }

    #[test]
    fn the_rights_are_the_windows_file_rights_and_no_role_denies() {
        let generic_read = 0x0012_0089;
        let generic_write = 0x0012_0116;
        let generic_execute = 0x0012_00a0;
        assert_eq!(READ_EXECUTE, generic_read | generic_execute);
        assert_eq!(
            MODIFY,
            generic_read | generic_write | generic_execute | DELETE
        );
        assert_eq!(
            MODIFY & (0x40 | 0x0004_0000 | 0x0008_0000),
            0,
            "modify never deletes children or changes access lists or owners"
        );
        assert_eq!(ANCHOR, 0x0012_01bf, "the folder-only split the farm proved");
        assert_eq!(ANCHOR & DELETE, 0, "an anchored folder cannot be renamed");
        assert_eq!(
            ANCHOR & ADD_ENTRIES,
            ADD_ENTRIES,
            "it still takes new entries"
        );
        assert_eq!(KEEP_ENTRIES, 0x0012_01b9);
        assert_eq!(KEEP_ENTRIES & ADD_ENTRIES, 0);
        assert_eq!(
            KEEP_ENTRIES & READ_EXECUTE,
            READ_EXECUTE,
            "a kept folder stays readable"
        );
        assert_eq!(Access::Hidden.rights(), Rights { this: 0, below: 0 });
        assert_eq!(
            Access::ReadOnly.rights(),
            Rights {
                this: READ_EXECUTE,
                below: READ_EXECUTE
            }
        );
        for access in [Access::Anchor, Access::RootAnchor] {
            assert_eq!(
                access.rights(),
                Rights {
                    this: ANCHOR,
                    below: MODIFY
                }
            );
        }
        for access in [Access::KeepEntries, Access::RootKeep] {
            assert_eq!(
                access.rights().below,
                MODIFY,
                "work below a kept folder goes on"
            );
        }
        assert!(Access::ReadOnly.cuts() && Access::Hidden.cuts());
        assert!(Access::Anchor.cuts() && Access::KeepEntries.cuts());
        assert!(!Access::RootAnchor.cuts() && !Access::RootKeep.cuts());
        assert!(!Access::Modify.cuts() && !Access::ReadExecute.cuts());
        assert!(!Access::Modify.narrows_others() && !Access::ReadExecute.narrows_others());
        assert!(Access::RootAnchor.narrows_others() && Access::Hidden.narrows_others());
    }

    #[test]
    fn guards_come_first_deepest_first_then_anchors_then_roots() {
        let ssh = r"C:\Users\mira\.ssh";
        let planned = plan(&profile(
            &[r"C:\Program Files\nodejs"],
            &[PROJECT, HOME, TMP],
            &[
                (CONTEXT, Guard::ReadOnly),
                (HOOKS, Guard::ReadOnly),
                (ssh, Guard::Hidden),
            ],
        ))
        .unwrap();
        assert_eq!(
            planned,
            [
                ace(HOOKS, Access::ReadOnly),
                ace(CONTEXT, Access::ReadOnly),
                ace(GIT, Access::Anchor),
                ace(PROJECT, Access::RootAnchor),
                ace(HOME, Access::Modify),
                ace(TMP, Access::Modify),
                ace(r"C:\Program Files\nodejs", Access::ReadExecute),
            ],
            "D1: a guarded path is cut before any grant that would reach it"
        );
        assert!(
            !planned.iter().any(|item| item.path == ssh),
            "a secret outside every grant gets no role"
        );
    }

    #[test]
    fn a_missing_submodule_store_keeps_dot_git_and_a_present_one_anchors_it() {
        let modules = r"C:\Users\mira\Projects\shop\.git\modules";
        let built = || profile(&[], &[PROJECT, HOME, TMP], &[(modules, Guard::ReadOnly)]);
        let present = super::plan(&built(), |_| true).unwrap();
        assert!(present.contains(&ace(modules, Access::ReadOnly)));
        assert!(present.contains(&ace(GIT, Access::Anchor)));
        let missing = super::plan(&built(), |path| path != modules).unwrap();
        assert!(
            missing.contains(&ace(GIT, Access::KeepEntries)),
            "a missing store keeps .git, the case the engine's pre-create avoids (F2)"
        );
        assert!(missing.contains(&ace(modules, Access::ReadOnly)));
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
        assert!(same(r"C:\A\b", r"c:\a\B"));
        assert!(!same(r"C:\A\b", r"C:\A"));
    }

    #[test]
    fn a_hidden_path_in_a_read_root_is_cut_and_a_read_only_one_needs_nothing() {
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
                ace(r"C:\Tools\keys", Access::Hidden),
                ace(PROJECT, Access::Modify),
                ace(r"C:\Tools", Access::ReadExecute),
            ],
            "a read root deletes nothing, so it needs no anchor"
        );
    }

    #[test]
    fn a_root_inside_a_protected_path_is_refused() {
        assert_eq!(
            plan(&profile(
                &[],
                &[PROJECT, CONTEXT],
                &[(CONTEXT, Guard::ReadOnly)]
            )),
            Err("a writable root sits in a protected path")
        );
        assert_eq!(
            plan(&profile(&[], &[CONTEXT], &[(PROJECT, Guard::Hidden)])),
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
    fn folders_holding_a_hidden_path_are_anchored_and_a_missing_one_keeps_its_folder() {
        let config = r"C:\Users\mira\Projects\shop\.config";
        let keys = r"C:\Users\mira\Projects\shop\.config\keys";
        let key = r"C:\Users\mira\Projects\shop\.config\keys\deploy.pem";
        let cache = format!(r"{HOME}\cache");
        let token = format!(r"{HOME}\cache\token");
        let built = || {
            profile(
                &[r"C:\Tools"],
                &[PROJECT, HOME, TMP],
                &[
                    (key, Guard::Hidden),
                    (token.as_str(), Guard::Hidden),
                    (r"C:\Tools\keys", Guard::Hidden),
                ],
            )
        };
        let present = plan(&built()).unwrap();
        assert_eq!(
            roles(&present, Access::Hidden),
            [token.as_str(), key, r"C:\Tools\keys"]
        );
        assert_eq!(
            roles(&present, Access::Anchor),
            [cache.as_str(), keys, config]
        );
        assert_eq!(roles(&present, Access::RootAnchor), [PROJECT, HOME]);
        assert!(roles(&present, Access::KeepEntries).is_empty());
        assert!(
            !present
                .iter()
                .any(|item| item.path.starts_with(r"C:\Tools") && item.access == Access::Anchor)
        );
        let missing = super::plan(&built(), |path| path != key).unwrap();
        assert!(missing.contains(&ace(keys, Access::KeepEntries)));
        assert!(missing.contains(&ace(config, Access::Anchor)));
        let gone = super::plan(&built(), |path| path != keys && path != key).unwrap();
        assert!(gone.contains(&ace(config, Access::KeepEntries)));
        assert!(!gone.iter().any(|item| item.path == keys));
    }

    #[test]
    fn folders_holding_a_read_only_path_are_anchored_so_it_cannot_be_vacated() {
        let protected = [
            CONTEXT,
            r"C:\Users\mira\Projects\shop\.crazyagent\context",
            HOOKS,
            CONFIG,
        ];
        let guarded: Vec<(&str, Guard)> = protected
            .iter()
            .map(|path| (*path, Guard::ReadOnly))
            .chain([(r"C:\Users\mira\Projects\shop\.git\secret", Guard::Hidden)])
            .collect();
        let planned = plan(&profile(&[], &[PROJECT, HOME, TMP], &guarded)).unwrap();
        assert_eq!(roles(&planned, Access::Anchor), [STATE, GIT]);
        assert_eq!(roles(&planned, Access::RootAnchor), [PROJECT]);
        assert!(roles(&planned, Access::KeepEntries).is_empty());
        for path in protected {
            assert!(planned.contains(&ace(path, Access::ReadOnly)));
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
            confined.iter().all(|item| item.access == Access::Modify),
            "read-only paths outside every writable root need no role"
        );
    }

    #[test]
    fn a_missing_guarded_path_keeps_the_entries_of_the_deepest_folder_above_it_only() {
        let guarded = [
            (CONTEXT, Guard::ReadOnly),
            (
                r"C:\Users\mira\Projects\shop\.crazyagent\context",
                Guard::ReadOnly,
            ),
            (HOOKS, Guard::ReadOnly),
            (CONFIG, Guard::ReadOnly),
        ];
        let without = |gone: &'static str| move |path: &str| !within(path, gone);
        let folders = |planned: Vec<Ace>| -> Vec<(String, Access)> {
            planned
                .into_iter()
                .filter(|item| !matches!(item.access, Access::ReadOnly | Access::Modify))
                .map(|item| (item.path, item.access))
                .collect()
        };
        let profile = profile(&[], &[PROJECT, HOME, TMP], &guarded);
        assert_eq!(
            folders(super::plan(&profile, without(HOOKS)).unwrap()),
            [
                (STATE.to_owned(), Access::Anchor),
                (GIT.to_owned(), Access::KeepEntries),
                (PROJECT.to_owned(), Access::RootAnchor),
            ],
            "only .git stops new entries, so the missing hooks folder cannot be created"
        );
        assert_eq!(
            folders(super::plan(&profile, without(STATE)).unwrap()),
            [
                (GIT.to_owned(), Access::Anchor),
                (PROJECT.to_owned(), Access::RootKeep),
            ],
            "a missing folder on the way keeps the entries of the present one above it"
        );
        assert_eq!(
            folders(super::plan(&profile, without(GIT)).unwrap()),
            [
                (STATE.to_owned(), Access::Anchor),
                (PROJECT.to_owned(), Access::RootKeep),
            ],
            "a project without .git cannot be given one with hooks or a config"
        );
        assert_eq!(
            folders(super::plan(&profile, without(CONTEXT)).unwrap()),
            [
                (STATE.to_owned(), Access::Anchor),
                (GIT.to_owned(), Access::Anchor),
                (PROJECT.to_owned(), Access::RootKeep),
            ]
        );
    }

    #[test]
    fn a_path_inside_a_guarded_folder_needs_no_anchor_and_hidden_wins_over_read_only() {
        let keys = r"C:\Users\mira\Projects\shop\keys";
        let planned = plan(&profile(
            &[],
            &[PROJECT],
            &[
                (keys, Guard::ReadOnly),
                (keys, Guard::Hidden),
                (r"C:\Users\mira\Projects\shop\KEYS", Guard::ReadOnly),
            ],
        ))
        .unwrap();
        assert_eq!(
            planned,
            [ace(keys, Access::Hidden), ace(PROJECT, Access::RootAnchor)]
        );
        let nested = plan(&profile(
            &[],
            &[PROJECT],
            &[
                (HOOKS, Guard::ReadOnly),
                (
                    r"C:\Users\mira\Projects\shop\.git\hooks\deep\secret",
                    Guard::Hidden,
                ),
            ],
        ))
        .unwrap();
        assert_eq!(
            nested,
            [
                ace(
                    r"C:\Users\mira\Projects\shop\.git\hooks\deep\secret",
                    Access::Hidden
                ),
                ace(HOOKS, Access::ReadOnly),
                ace(GIT, Access::Anchor),
                ace(PROJECT, Access::RootAnchor),
            ],
            "inside a read-only folder nothing can be deleted or added, so the walk stops there"
        );
    }
}
