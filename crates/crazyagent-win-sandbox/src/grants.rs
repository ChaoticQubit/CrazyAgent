use crate::plan::{Access, Ace, same};
use crate::profile::check_path;

/// The first line of every grants record, naming its format and version: version 2 records
/// roles that only allow (DEC-193), and any other version is refused.
pub const HEADER: &str = "crazyagent-win-grants 2";
/// The most entries one record holds: the plans of two profiles, each far below it.
pub const ENTRIES_MAX: usize = 4096;
/// The largest record the helper reads: every entry at the longest path a profile allows.
pub const RECORD_BYTES_MAX: usize = ENTRIES_MAX * 1040;

const ACCESSES: [(Access, &str); 8] = [
    (Access::ReadExecute, "readexecute"),
    (Access::Modify, "modify"),
    (Access::RootAnchor, "rootanchor"),
    (Access::RootKeep, "rootkeep"),
    (Access::ReadOnly, "readonly"),
    (Access::Hidden, "hidden"),
    (Access::Anchor, "anchor"),
    (Access::KeepEntries, "keepentries"),
];

fn keyword(access: Access) -> &'static str {
    let (_, word) = ACCESSES
        .iter()
        .find(|(known, _)| *known == access)
        .expect("every access has a keyword");
    word
}

/// Writes the access entries the boundary put on paths for one container as text: the
/// header, then one `keyword path` line per entry, each ending in a newline. Input: the
/// entries. Output: the text, or the refusal when there are more than 4096.
pub fn encode(entries: &[Ace]) -> Result<String, &'static str> {
    if entries.len() > ENTRIES_MAX {
        return Err("too many granted paths to record");
    }
    let mut text = format!("{HEADER}\n");
    for entry in entries {
        assert_eq!(check_path(&entry.path), Ok(()));
        text.push_str(&format!("{} {}\n", keyword(entry.access), entry.path));
    }
    assert!(text.len() <= RECORD_BYTES_MAX);
    Ok(text)
}

/// Reads a record written by [`encode`], refusing anything else rather than guessing which
/// paths still hold a grant: the header first, known keywords only, plain drive paths, no
/// empty or carriage-return lines, at most 4096 entries. Input: the text. Output: the
/// entries, or the refusal.
pub fn decode(text: &str) -> Result<Vec<Ace>, &'static str> {
    const DAMAGED: &str = "the record of granted paths is damaged";
    if text.len() > RECORD_BYTES_MAX {
        return Err(DAMAGED);
    }
    let body = text.strip_suffix('\n').ok_or(DAMAGED)?;
    let mut lines = body.split('\n');
    if lines.next() != Some(HEADER) {
        return Err(DAMAGED);
    }
    let mut entries = Vec::new();
    for line in lines {
        let (word, path) = line.split_once(' ').ok_or(DAMAGED)?;
        let (access, _) = ACCESSES
            .iter()
            .find(|(_, known)| *known == word)
            .ok_or(DAMAGED)?;
        check_path(path).map_err(|_| DAMAGED)?;
        if entries.len() == ENTRIES_MAX {
            return Err(DAMAGED);
        }
        entries.push(Ace {
            path: path.to_owned(),
            access: *access,
        });
    }
    assert!(entries.len() <= ENTRIES_MAX);
    Ok(entries)
}

/// The recorded paths to withdraw before the new plan applies: a path the plan no longer
/// names, and a path whose inheritance a recorded role cut where the plan names one that does
/// not cut, so its own access list comes back first. Any other change of role is made in
/// place when the plan applies. Paths compare without regard to case, as Windows does; each
/// comes once, in record order. Inputs: the record and the new plan. Output: the paths.
pub fn withdrawn<'a>(recorded: &'a [Ace], planned: &[Ace]) -> Vec<&'a str> {
    let mut paths: Vec<&'a str> = Vec::new();
    for entry in recorded {
        let role = planned.iter().find(|plan| same(&plan.path, &entry.path));
        let kept = role.is_some_and(|plan| plan.access.cuts() || !entry.access.cuts());
        if !kept && !paths.iter().any(|path| same(path, &entry.path)) {
            paths.push(&entry.path);
        }
    }
    assert!(paths.len() <= recorded.len());
    assert!(
        paths
            .iter()
            .all(|path| recorded.iter().any(|entry| entry.path == *path))
    );
    paths
}

/// The record kept while the boundary changes from one plan to the next: every recorded
/// entry, then each planned entry not already in it, so a launch interrupted at any step
/// still knows every path that may hold a grant. Inputs: the record and the new plan.
/// Output: both lists merged.
pub fn union(recorded: &[Ace], planned: &[Ace]) -> Vec<Ace> {
    let mut entries = recorded.to_vec();
    for entry in planned {
        if !entries.contains(entry) {
            entries.push(entry.clone());
        }
    }
    assert!(entries.len() <= recorded.len() + planned.len());
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECT: &str = r"C:\Users\mira\Projects\shop";
    const CONTEXT: &str = r"C:\Users\mira\Projects\shop\context";
    const GIT: &str = r"C:\Users\mira\Projects\shop\.git";
    const DOCUMENTS: &str = r"C:\Users\mira\Documents";
    const TOOLS: &str = r"C:\Tools";

    fn ace(path: &str, access: Access) -> Ace {
        Ace {
            path: path.into(),
            access,
        }
    }

    #[test]
    fn a_record_written_reads_back_the_same() {
        let record = [
            ace(CONTEXT, Access::ReadOnly),
            ace(r"C:\Users\mira\Projects\shop\keys", Access::Hidden),
            ace(GIT, Access::Anchor),
            ace(
                r"C:\Users\mira\Projects\shop\.crazyagent",
                Access::KeepEntries,
            ),
            ace(PROJECT, Access::RootAnchor),
            ace(DOCUMENTS, Access::RootKeep),
            ace(r"C:\Users\mira\Downloads", Access::Modify),
            ace(TOOLS, Access::ReadExecute),
        ];
        assert_eq!(record.len(), ACCESSES.len(), "every role once");
        let text = encode(&record).unwrap();
        assert!(text.starts_with("crazyagent-win-grants 2\n"));
        assert!(text.contains("\nreadonly C:\\Users\\mira\\Projects\\shop\\context\n"));
        assert!(text.contains("\nrootanchor C:\\Users\\mira\\Projects\\shop\n"));
        assert!(text.contains("\nrootkeep C:\\Users\\mira\\Documents\n"));
        assert!(text.ends_with('\n'));
        assert_eq!(decode(&text).unwrap(), record);
        assert_eq!(encode(&[]).unwrap(), format!("{HEADER}\n"));
        assert!(decode(&encode(&[]).unwrap()).unwrap().is_empty());
    }

    #[test]
    fn a_damaged_record_is_refused_not_guessed() {
        let good = encode(&[ace(PROJECT, Access::Modify)]).unwrap();
        for bad in [
            String::new(),
            "crazyagent-win-grants 1\n".to_owned(),
            good.replace("grants 2", "grants 1"),
            good.replace("modify", "denywrite"),
            good.replace("modify", "own"),
            good.replace(PROJECT, r"Projects\shop"),
            good.replace('\n', "\r\n"),
            format!("{good}\n"),
            good.replace("modify ", "modify"),
            good.replace(PROJECT, ""),
        ] {
            assert!(decode(&bad).is_err(), "{bad:?}");
        }
        let many: Vec<Ace> = (0..=ENTRIES_MAX)
            .map(|n| ace(&format!(r"C:\r{n}"), Access::Modify))
            .collect();
        assert!(encode(&many).is_err());
        assert!(encode(&many[..ENTRIES_MAX]).is_ok());
        let listed: String = (0..=ENTRIES_MAX)
            .map(|n| format!("modify C:\\r{n}\n"))
            .collect();
        assert!(decode(&format!("{HEADER}\n{listed}")).is_err());
        assert!(decode(&"x".repeat(RECORD_BYTES_MAX + 1)).is_err());
    }

    #[test]
    fn a_path_is_withdrawn_when_it_leaves_the_plan_or_its_cut_ends() {
        let before = [
            ace(CONTEXT, Access::ReadOnly),
            ace(GIT, Access::Anchor),
            ace(PROJECT, Access::RootAnchor),
            ace(DOCUMENTS, Access::Modify),
            ace(TOOLS, Access::ReadExecute),
        ];
        let after = [
            ace(r"c:\users\MIRA\projects\shop\.GIT", Access::KeepEntries),
            ace(CONTEXT, Access::Modify),
            ace(r"c:\users\MIRA\projects\shop", Access::Modify),
            ace(DOCUMENTS, Access::ReadExecute),
            ace(r"C:\New", Access::Modify),
        ];
        assert_eq!(
            withdrawn(&before, &after),
            [CONTEXT, TOOLS],
            "a cut that ends and a dropped grant are withdrawn; a cut changed to another cut, \
             a root no longer anchored and a narrowed grant change in place"
        );
        assert!(withdrawn(&before, &before).is_empty());
        assert!(withdrawn(&[], &after).is_empty());
        assert_eq!(
            withdrawn(&before, &[]),
            [CONTEXT, GIT, PROJECT, DOCUMENTS, TOOLS]
        );
        let doubled = [
            ace(GIT, Access::Anchor),
            ace(r"C:\USERS\mira\Projects\shop\.git", Access::KeepEntries),
        ];
        assert_eq!(withdrawn(&doubled, &[]), [GIT], "each path once");
        assert!(
            withdrawn(&doubled, &[ace(GIT, Access::Hidden)]).is_empty(),
            "an interrupted change listing two cuts keeps the path cut"
        );
        assert!(
            withdrawn(
                &[ace(PROJECT, Access::Modify)],
                &[ace(PROJECT, Access::RootKeep)]
            )
            .is_empty(),
            "a root that starts to keep its entries needs no withdrawal"
        );
    }

    #[test]
    fn the_record_kept_during_a_change_lists_both_profiles() {
        let before = [ace(PROJECT, Access::Modify), ace(DOCUMENTS, Access::Modify)];
        let after = [
            ace(PROJECT, Access::Modify),
            ace(DOCUMENTS, Access::ReadExecute),
        ];
        assert_eq!(
            union(&before, &after),
            [
                ace(PROJECT, Access::Modify),
                ace(DOCUMENTS, Access::Modify),
                ace(DOCUMENTS, Access::ReadExecute),
            ]
        );
        assert_eq!(union(&[], &after), after);
    }
}
