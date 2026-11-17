use crate::dacl::{entries, explicit, package, without};
use crate::plan::same;
use crate::profile::check_path;

/// The first line of the shared record of guarded paths, naming its format and version.
pub const HEADER: &str = "crazyagent-win-protected 1";
/// The most guarded paths one record holds: a few per profile, for every agent on the machine.
pub const PATHS_MAX: usize = 256;
/// The longest access list kept for a guarded path; a path with a longer list is not guarded.
pub const LIST_BYTES_MAX: usize = 8 * 1024;
/// The largest record the helper reads: every path with two lists at the longest size.
pub const RECORD_BYTES_MAX: usize = PATHS_MAX * (4 * LIST_BYTES_MAX + 1024 + 16);

/// What the boundary keeps for one guarded path, shared by every container that guards it:
/// whether its list was protected and its explicit entries before the first cut, the list the
/// boundary last wrote, and whether the owner changed the list while it was guarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub path: String,
    pub protected: bool,
    pub original: Vec<u8>,
    pub set: Vec<u8>,
    pub changed: bool,
}

/// What releasing a guarded path does: the list to write (None leaves it), the protection to
/// put back (None keeps the current one), the snapshot to keep (None removes its line), and
/// whether the owner's own list was kept, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub list: Option<Vec<u8>>,
    pub protected: Option<bool>,
    pub snapshot: Option<Snapshot>,
    pub owner_changed: bool,
}

/// The snapshot to save before a container cuts and sets a guarded path, called while the
/// shared lock is held. A path seen for the first time keeps its protection and explicit
/// entries as they are now, less the container's own, such as an older boundary's deny. A path
/// already guarded is marked changed when its list is neither what the boundary last wrote
/// nor its uncut original, as when the owner edited it. Inputs: the saved snapshot if any, the
/// path, the container SID, whether its list is protected now, its list now and the list
/// about to be written. Output: the snapshot, or the refusal when a list is malformed or
/// longer than the boundary keeps.
pub fn guard(
    saved: Option<&Snapshot>,
    path: &str,
    sid: &[u8],
    protected: bool,
    current: &[u8],
    target: &[u8],
) -> Result<Snapshot, &'static str> {
    assert_eq!(check_path(path), Ok(()));
    if current.len() > LIST_BYTES_MAX || target.len() > LIST_BYTES_MAX {
        return Err("a guarded path's access list is too long to keep");
    }
    assert!(package(sid));
    entries(target)?;
    let found = explicit(current)?;
    let found = without(&found, sid)?.unwrap_or(found);
    let Some(saved) = saved else {
        return Ok(Snapshot {
            path: path.to_owned(),
            protected,
            original: found,
            set: target.to_vec(),
            changed: false,
        });
    };
    assert!(same(&saved.path, path));
    let uncut = protected == saved.protected && found == saved.original;
    let changed = saved.changed || (current != saved.set.as_slice() && !uncut);
    Ok(Snapshot {
        set: target.to_vec(),
        changed,
        ..saved.clone()
    })
}

/// What releasing one container's guard on a path does, called while the shared lock is held.
/// While another container still guards the path, only this container's entries go and the
/// path stays cut. When this was the last guard, the original list and protection come back
/// only if the path still holds exactly what the boundary last wrote and the owner never
/// changed it; otherwise the owner's list stays with only this container's entries taken out.
/// Inputs: the snapshot, the path's list now, the container SID and whether another
/// container's record still guards the path. Output: the release, or the refusal for a
/// malformed list.
pub fn release(
    saved: &Snapshot,
    current: &[u8],
    sid: &[u8],
    guarded_by_others: bool,
) -> Result<Release, &'static str> {
    assert!(package(sid));
    assert_eq!(check_path(&saved.path), Ok(()));
    let withdrawn = without(current, sid)?;
    let changed = saved.changed || current != saved.set.as_slice();
    if guarded_by_others {
        let set = withdrawn.clone().unwrap_or_else(|| current.to_vec());
        return Ok(Release {
            list: withdrawn,
            protected: None,
            snapshot: Some(Snapshot {
                set,
                changed,
                ..saved.clone()
            }),
            owner_changed: false,
        });
    }
    if changed {
        return Ok(Release {
            list: withdrawn,
            protected: None,
            snapshot: None,
            owner_changed: true,
        });
    }
    Ok(Release {
        list: Some(saved.original.clone()),
        protected: Some(saved.protected),
        snapshot: None,
        owner_changed: false,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || text.len() > 2 * LIST_BYTES_MAX {
        return None;
    }
    let digit = |unit: u8| match unit {
        b'0'..=b'9' => Some(unit - b'0'),
        b'a'..=b'f' => Some(unit - b'a' + 10),
        _ => None,
    };
    text.as_bytes()
        .chunks(2)
        .map(|pair| Some((digit(pair[0])? << 4) | digit(pair[1])?))
        .collect()
}

fn flag(word: &str) -> Option<bool> {
    match word {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

/// Writes the shared record as text: the header, then one line per path with its protection,
/// changed flag, original explicit list and last written list in lowercase hex, and the path
/// last, since a path may hold spaces. Input: the snapshots. Output: the text, or the refusal
/// when there are more than 256 paths.
pub fn encode(snapshots: &[Snapshot]) -> Result<String, &'static str> {
    if snapshots.len() > PATHS_MAX {
        return Err("too many guarded paths to record");
    }
    let mut text = format!("{HEADER}\n");
    for snapshot in snapshots {
        assert_eq!(check_path(&snapshot.path), Ok(()));
        assert!(snapshot.original.len() <= LIST_BYTES_MAX);
        assert!(snapshot.set.len() <= LIST_BYTES_MAX);
        text.push_str(&format!(
            "{} {} {} {} {}\n",
            u8::from(snapshot.protected),
            u8::from(snapshot.changed),
            hex(&snapshot.original),
            hex(&snapshot.set),
            snapshot.path
        ));
    }
    assert!(text.len() <= RECORD_BYTES_MAX);
    Ok(text)
}

/// Reads a record written by [`encode`], refusing anything else rather than guessing what a
/// guarded path held: the header first, flags 0 or 1, well-formed access lists in lowercase
/// hex, plain drive paths, each path once, at most 256 lines. Input: the text. Output: the
/// snapshots, or the refusal.
pub fn decode(text: &str) -> Result<Vec<Snapshot>, &'static str> {
    const DAMAGED: &str = "the record of guarded paths is damaged";
    if text.len() > RECORD_BYTES_MAX {
        return Err(DAMAGED);
    }
    let body = text.strip_suffix('\n').ok_or(DAMAGED)?;
    let mut lines = body.split('\n');
    if lines.next() != Some(HEADER) {
        return Err(DAMAGED);
    }
    let mut snapshots: Vec<Snapshot> = Vec::new();
    for line in lines {
        let mut fields = line.splitn(5, ' ');
        let mut next = || fields.next().ok_or(DAMAGED);
        let protected = flag(next()?).ok_or(DAMAGED)?;
        let changed = flag(next()?).ok_or(DAMAGED)?;
        let original = unhex(next()?).ok_or(DAMAGED)?;
        let set = unhex(next()?).ok_or(DAMAGED)?;
        let path = next()?;
        check_path(path).map_err(|_| DAMAGED)?;
        entries(&original).map_err(|_| DAMAGED)?;
        entries(&set).map_err(|_| DAMAGED)?;
        if snapshots.len() == PATHS_MAX || snapshots.iter().any(|seen| same(&seen.path, path)) {
            return Err(DAMAGED);
        }
        snapshots.push(Snapshot {
            path: path.to_owned(),
            protected,
            original,
            set,
            changed,
        });
    }
    assert!(snapshots.len() <= PATHS_MAX);
    Ok(snapshots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dacl::converge;
    use crate::plan::Access;

    const HOOKS: &str = r"C:\Users\mira\Projects\shop\.git\hooks";
    const CONTAINER: [u8; 20] = [1, 3, 0, 0, 0, 0, 0, 15, 2, 0, 0, 0, 7, 0, 0, 0, 9, 0, 0, 0];
    const TEAMMATE: [u8; 20] = [1, 3, 0, 0, 0, 0, 0, 15, 2, 0, 0, 0, 8, 0, 0, 0, 4, 0, 0, 0];
    const OWNER: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];

    fn ace(kind: u8, flags: u8, mask: u32, sid: &[u8]) -> Vec<u8> {
        let length = (8 + sid.len()) as u16;
        let mut bytes = vec![kind, flags];
        bytes.extend(length.to_le_bytes());
        bytes.extend(mask.to_le_bytes());
        bytes.extend(sid);
        bytes
    }

    fn acl(aces: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = aces.concat();
        let mut bytes = vec![2, 0];
        bytes.extend(((8 + body.len()) as u16).to_le_bytes());
        bytes.extend((aces.len() as u16).to_le_bytes());
        bytes.extend([0, 0]);
        bytes.extend(body);
        bytes
    }

    fn uncut() -> Vec<u8> {
        acl(&[
            ace(0, 0, 0x2, &OWNER),
            ace(0, 0x13, 0x001f_01ff, &OWNER),
            ace(0, 0x13, 0x0013_01bf, &CONTAINER),
        ])
    }

    fn cut() -> Vec<u8> {
        converge(&uncut(), &CONTAINER, Access::ReadOnly, true)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn a_first_guard_keeps_the_explicit_entries_and_protection_it_found() {
        let snapshot = guard(None, HOOKS, &CONTAINER, false, &uncut(), &cut()).unwrap();
        assert_eq!(
            snapshot,
            Snapshot {
                path: HOOKS.into(),
                protected: false,
                original: acl(&[ace(0, 0, 0x2, &OWNER)]),
                set: cut(),
                changed: false,
            }
        );
        let again = guard(Some(&snapshot), HOOKS, &CONTAINER, true, &cut(), &cut()).unwrap();
        assert_eq!(
            again, snapshot,
            "a path still at what was written stays unchanged"
        );
        let interrupted =
            guard(Some(&snapshot), HOOKS, &CONTAINER, false, &uncut(), &cut()).unwrap();
        let mut older = uncut();
        older.splice(8..8, ace(1, 0, 0x000d_0156, &CONTAINER));
        older[4] += 1;
        let size = u16::try_from(older.len()).unwrap();
        older[2..4].copy_from_slice(&size.to_le_bytes());
        assert_eq!(
            guard(None, HOOKS, &CONTAINER, false, &older, &cut())
                .unwrap()
                .original,
            snapshot.original,
            "an older boundary's own deny is not kept to come back at release"
        );
        assert!(
            !interrupted.changed,
            "a launch stopped before its cut finds the uncut original, not an owner edit"
        );
    }

    #[test]
    fn an_owner_edit_while_guarded_is_remembered_until_release() {
        let snapshot = guard(None, HOOKS, &CONTAINER, false, &uncut(), &cut()).unwrap();
        let edited = acl(&[ace(0, 0x3, 0x001f_01ff, &OWNER)]);
        let marked = guard(Some(&snapshot), HOOKS, &CONTAINER, true, &edited, &cut()).unwrap();
        assert!(marked.changed);
        assert_eq!(marked.set, cut());
        let kept = guard(Some(&marked), HOOKS, &CONTAINER, true, &cut(), &cut()).unwrap();
        assert!(
            kept.changed,
            "the mark stays after the boundary writes again"
        );
        let released = release(&kept, &cut(), &CONTAINER, false).unwrap();
        assert_eq!(
            released,
            Release {
                list: without(&cut(), &CONTAINER).unwrap(),
                protected: None,
                snapshot: None,
                owner_changed: true,
            },
            "D3: the owner's list stays and only the container's entries go"
        );
    }

    #[test]
    fn the_last_release_restores_an_untouched_path() {
        let snapshot = guard(None, HOOKS, &CONTAINER, false, &uncut(), &cut()).unwrap();
        assert_eq!(
            release(&snapshot, &cut(), &CONTAINER, false).unwrap(),
            Release {
                list: Some(acl(&[ace(0, 0, 0x2, &OWNER)])),
                protected: Some(false),
                snapshot: None,
                owner_changed: false,
            }
        );
        let edited = acl(&[ace(0, 0x3, 0x001f_01ff, &OWNER)]);
        let after_edit = release(&snapshot, &edited, &CONTAINER, false).unwrap();
        assert!(after_edit.owner_changed);
        assert_eq!(
            after_edit.list, None,
            "an edit without the container's entry is left alone"
        );
    }

    #[test]
    fn a_release_while_a_teammate_guards_takes_only_its_own_entries() {
        let both = converge(&cut(), &TEAMMATE, Access::ReadOnly, true)
            .unwrap()
            .unwrap();
        let first = guard(None, HOOKS, &CONTAINER, false, &uncut(), &cut()).unwrap();
        let shared = guard(Some(&first), HOOKS, &TEAMMATE, true, &cut(), &both).unwrap();
        assert!(!shared.changed);
        let released = release(&shared, &both, &CONTAINER, true).unwrap();
        let left = without(&both, &CONTAINER).unwrap().unwrap();
        assert_eq!(released.list, Some(left.clone()));
        assert_eq!(released.protected, None, "the path stays cut");
        assert!(!released.owner_changed);
        let kept = released.snapshot.unwrap();
        assert_eq!(kept.set, left);
        assert_eq!(kept.original, first.original);
        assert_eq!(
            release(&kept, &left, &TEAMMATE, false).unwrap().protected,
            Some(false),
            "the teammate's last release restores the path"
        );
    }

    #[test]
    fn a_record_written_reads_back_the_same_and_damage_is_refused() {
        let first = guard(None, HOOKS, &CONTAINER, false, &uncut(), &cut()).unwrap();
        let second = Snapshot {
            path: r"C:\Users\mira\Projects\shop\my context".into(),
            protected: true,
            changed: true,
            ..first.clone()
        };
        let text = encode(&[first.clone(), second.clone()]).unwrap();
        assert!(text.starts_with("crazyagent-win-protected 1\n"));
        assert!(text.ends_with(" C:\\Users\\mira\\Projects\\shop\\my context\n"));
        assert_eq!(decode(&text).unwrap(), [first.clone(), second]);
        assert_eq!(decode(&encode(&[]).unwrap()).unwrap(), []);
        let good = encode(std::slice::from_ref(&first)).unwrap();
        for bad in [
            String::new(),
            good.replace(HEADER, "crazyagent-win-protected 2"),
            good.replacen("\n0 0 ", "\n2 0 ", 1),
            good.replacen("\n0 0 ", "\n0 x ", 1),
            good.replace(HOOKS, r"Projects\shop\.git\hooks"),
            good.replace('\n', "\r\n"),
            format!("{good}\n"),
            good.replacen(&hex(&first.set), &hex(&first.set).to_uppercase(), 1),
            good.replacen(&hex(&first.set), &hex(&first.set)[..10], 1),
            good.replacen(&hex(&first.set), "0", 1),
            format!(
                "{good}{}\n",
                good.lines()
                    .nth(1)
                    .unwrap()
                    .replace(HOOKS, &HOOKS.to_uppercase())
            ),
        ] {
            assert!(decode(&bad).is_err(), "{bad:?}");
        }
        assert!(decode(&"x".repeat(RECORD_BYTES_MAX + 1)).is_err());
        let many: Vec<Snapshot> = (0..=PATHS_MAX)
            .map(|n| Snapshot {
                path: format!(r"C:\p{n}"),
                ..first.clone()
            })
            .collect();
        assert!(encode(&many).is_err());
        assert!(encode(&many[..PATHS_MAX]).is_ok());
    }

    #[test]
    fn a_list_longer_than_the_record_keeps_is_not_guarded() {
        let long: Vec<Vec<u8>> = (0..=(LIST_BYTES_MAX / 20))
            .map(|_| ace(0, 0, 1, &OWNER))
            .collect();
        let list = acl(&long);
        assert!(list.len() > LIST_BYTES_MAX);
        assert!(guard(None, HOOKS, &CONTAINER, false, &list, &cut()).is_err());
        assert!(guard(None, HOOKS, &CONTAINER, false, &uncut(), &list).is_err());
    }
}
